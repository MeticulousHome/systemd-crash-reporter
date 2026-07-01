use chrono::Utc;
use sentry::{
    Client, Hub, Scope,
    protocol::{Attachment, AttachmentType},
    types::Dsn,
};
use std::env;
use std::fs;
use std::process::Command;
use std::sync::Arc;

const BUILD_VERSION_PATH: &str = "/opt/image-build-version";
const BUILD_DATE_PATH: &str = "/opt/ROOTFS_BUILD_DATE";
const SERVICE_LOG_ATTACHMENT_BYTES: usize = 256 * 1024;
const MEMORY_DIAGNOSTICS_ATTACHMENT_BYTES: usize = 256 * 1024;
const GENERAL_JOURNAL_ATTACHMENT_BYTES: usize = 256 * 1024;
const HIGH_MEMORY_SERVICE_LOG_ATTACHMENT_BYTES: usize = 256 * 1024;
const SERVICE_MEMORY_RANKING_SCRIPT: &str = r#"
systemctl list-units --type=service --all --plain --no-legend |
awk '{print $1}' |
while read -r unit; do
    [ -n "$unit" ] || continue
    peak=$(systemctl show "$unit" --property=MemoryPeak --value 2>/dev/null)
    current=$(systemctl show "$unit" --property=MemoryCurrent --value 2>/dev/null)
    case "$peak" in ''|'[not set]'|*[!0-9]*) peak=0 ;; esac
    case "$current" in ''|'[not set]'|*[!0-9]*) current=0 ;; esac
    printf "%s\t%s\t%s\n" "$peak" "$current" "$unit"
done |
sort -rn -k1,1 -k2,2 |
head -n 20
"#;

fn run_command(title: &str, program: &str, args: &[&str]) -> String {
    let mut section = format!("\n\n=============== {title} ===============\n\n");

    match Command::new(program).args(args).output() {
        Ok(output) => {
            section.push_str(&format!("$ {} {}\n", program, args.join(" ")));
            section.push_str(&format!("exit_status: {}\n\n", output.status));

            if !output.stdout.is_empty() {
                section.push_str(&String::from_utf8_lossy(&output.stdout));
                if !section.ends_with('\n') {
                    section.push('\n');
                }
            }

            if !output.stderr.is_empty() {
                section.push_str("\n--- stderr ---\n");
                section.push_str(&String::from_utf8_lossy(&output.stderr));
                if !section.ends_with('\n') {
                    section.push('\n');
                }
            }
        }
        Err(e) => {
            section.push_str(&format!("failed to run {}: {}\n", program, e));
        }
    }

    section
}

fn truncate_for_attachment(text: String, max_bytes: usize) -> Vec<u8> {
    let bytes = text.into_bytes();
    if bytes.len() <= max_bytes {
        return bytes;
    }

    let marker = b"\n\n[crash-reporter truncated attachment]\n\n";
    let head_len = max_bytes / 3;
    let tail_len = max_bytes - head_len - marker.len();

    let mut truncated = Vec::with_capacity(max_bytes);
    truncated.extend_from_slice(&bytes[..head_len]);
    truncated.extend_from_slice(marker);
    truncated.extend_from_slice(&bytes[bytes.len() - tail_len..]);
    truncated
}

fn read_or_unknown(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|cause| {
        println!("Error {} while reading file at: {}", cause, path);
        String::from("unknown")
    })
}

fn collect_service_logs(unit: &str) -> Vec<u8> {
    let mut logs = format!(
        "\n=============== LAST LOGS FROM {} ===============\n\n",
        unit.to_ascii_uppercase()
    );
    logs.push_str(&run_command(
        "failed service journal",
        "journalctl",
        &[
            "--no-pager",
            "--output=short-iso",
            "--unit",
            unit,
            "--since=10 minutes ago",
            "--lines=300",
        ],
    ));

    truncate_for_attachment(logs, SERVICE_LOG_ATTACHMENT_BYTES)
}

fn collect_general_journal_logs(failed_unit: &str) -> Vec<u8> {
    let mut logs = format!(
        "\n=============== GENERAL JOURNAL FOR OOM CRASH ===============\n\nfailed_unit: {failed_unit}\n\n"
    );
    logs.push_str(&run_command(
        "general journal",
        "journalctl",
        &[
            "--no-pager",
            "--output=short-iso",
            "--since=10 minutes ago",
            "--lines=300",
        ],
    ));

    truncate_for_attachment(logs, GENERAL_JOURNAL_ATTACHMENT_BYTES)
}

fn is_oom_failure(job_result: &str, exit_status: &str) -> bool {
    job_result.eq_ignore_ascii_case("oom-kill") || exit_status.eq_ignore_ascii_case("oom-kill")
}

fn highest_memory_peak_service() -> Option<String> {
    let output = Command::new("sh")
        .args(["-c", SERVICE_MEMORY_RANKING_SCRIPT])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut fields = line.split('\t');
        let peak = fields.next()?.parse::<u64>().ok()?;
        let current = fields.next()?.parse::<u64>().ok()?;
        let unit = fields.next()?.trim();

        if (peak > 0 || current > 0) && unit.ends_with(".service") {
            return Some(unit.to_string());
        }
    }

    None
}

fn collect_high_memory_service_logs(failed_unit: &str) -> Option<(String, Vec<u8>)> {
    let high_memory_unit = highest_memory_peak_service()?;
    if high_memory_unit == failed_unit {
        return None;
    }

    let mut logs = format!(
        "\n=============== HIGH MEMORY SERVICE LOGS FOR OOM CRASH ===============\n\nfailed_unit: {failed_unit}\nhigh_memory_unit: {high_memory_unit}\n\n"
    );
    logs.push_str(&run_command(
        "high memory service state",
        "systemctl",
        &["status", "--no-pager", "--lines=80", &high_memory_unit],
    ));
    logs.push_str(&run_command(
        "high memory service journal",
        "journalctl",
        &[
            "--no-pager",
            "--output=short-iso",
            "--unit",
            &high_memory_unit,
            "--since=10 minutes ago",
            "--lines=150",
        ],
    ));

    Some((
        high_memory_unit,
        truncate_for_attachment(logs, HIGH_MEMORY_SERVICE_LOG_ATTACHMENT_BYTES),
    ))
}

fn collect_memory_diagnostics(
    unit: &str,
    hostname: &str,
    build_version: &str,
    build_date: &str,
    job_result: &str,
    exit_code: &str,
    exit_status: &str,
    invocation_id: &str,
) -> Vec<u8> {
    let mut report = format!(
        "\
=============== CRASH CONTEXT ===============

hostname: {hostname}
unit: {unit}
job_result: {job_result}
exit_code: {exit_code}
exit_status: {exit_status}
invocation_id: {invocation_id}
build_version: {}
build_date: {}
captured_at_utc: {}
",
        build_version.trim(),
        build_date.trim(),
        Utc::now().to_rfc3339()
    );

    report.push_str(&run_command("uptime", "uptime", &[]));
    report.push_str(&run_command("free", "free", &["-h"]));
    report.push_str(&run_command(
        "failed unit systemd state",
        "systemctl",
        &[
            "show",
            unit,
            "--property=Id",
            "--property=Description",
            "--property=LoadState",
            "--property=ActiveState",
            "--property=SubState",
            "--property=Result",
            "--property=ExecMainCode",
            "--property=ExecMainStatus",
            "--property=InvocationID",
            "--property=NRestarts",
            "--property=Restart",
            "--property=MemoryCurrent",
            "--property=MemoryPeak",
            "--property=MemoryHigh",
            "--property=MemoryMax",
            "--property=MemorySwapCurrent",
            "--property=OOMPolicy",
            "--property=CPUUsageNSec",
        ],
    ));
    report.push_str(&run_command(
        "meticulous slice memory",
        "systemctl",
        &[
            "show",
            "meticulous.slice",
            "--property=MemoryCurrent",
            "--property=MemoryPeak",
            "--property=MemoryHigh",
            "--property=MemoryMax",
            "--property=MemorySwapCurrent",
            "--property=CPUUsageNSec",
        ],
    ));
    report.push_str(&run_command(
        "service memory peak ranking",
        "sh",
        &["-c", SERVICE_MEMORY_RANKING_SCRIPT],
    ));
    report.push_str(&run_command(
        "top processes by rss",
        "sh",
        &[
            "-c",
            "ps -eo pid,ppid,user,stat,comm,rss,vsz,pmem,pcpu,args --sort=-rss | head -n 40",
        ],
    ));
    report.push_str(&run_command(
        "service process tree",
        "systemctl",
        &["status", "--no-pager", "--lines=80", unit],
    ));
    report.push_str(&run_command(
        "recent kernel oom lines",
        "sh",
        &[
            "-c",
            "journalctl --no-pager --output=short-iso --dmesg --since='30 minutes ago' | grep -Ei 'oom|out of memory|killed process|memory cgroup|invoked oom-killer' || true",
        ],
    ));
    report.push_str(&run_command(
        "recent systemd oomd lines",
        "journalctl",
        &[
            "--no-pager",
            "--output=short-iso",
            "--unit=systemd-oomd.service",
            "--since=30 minutes ago",
            "--lines=200",
        ],
    ));
    report.push_str(&run_command(
        "proc meminfo",
        "sh",
        &["-c", "cat /proc/meminfo"],
    ));

    truncate_for_attachment(report, MEMORY_DIAGNOSTICS_ATTACHMENT_BYTES)
}

fn main() {
    let _guard = sentry::init((
        "https://7d92061929211477cb44b8071be63441@sentry.meticulousespresso.com/4",
        sentry::ClientOptions {
            release: sentry::release_name!(),
            send_default_pii: false,
            ..Default::default()
        },
    ));

    let sc_dsn: Dsn = "https://295725e5bbfc9b3eb0413cafc1f6cea6@o4506723336060928.ingest.us.sentry.io/4509197049593856".parse().unwrap();

    let mut sc_opts = sentry::ClientOptions {
        debug: true,
        release: sentry::release_name!(),
        send_default_pii: false,
        ..Default::default()
    };
    sc_opts.dsn = Some(sc_dsn);

    sc_opts.transport = Some(Arc::new(sentry::transports::DefaultTransportFactory));

    let secondary_client = Some(Arc::new(Client::from_config(sc_opts)));

    // Extract environment variables provided by systemd
    let unit = env::var("MONITOR_UNIT").unwrap_or_else(|_| "unknown".into());
    let job_result = env::var("MONITOR_SERVICE_RESULT").unwrap_or_else(|_| "unknown".into());
    let exit_code = env::var("MONITOR_EXIT_CODE").unwrap_or_else(|_| "unknown".into());
    let exit_status = env::var("MONITOR_EXIT_STATUS").unwrap_or_else(|_| "unknown".into());
    let invocation_id = env::var("MONITOR_INVOCATION_ID").unwrap_or_else(|_| "unknown".into());
    // Get the machine's hostname
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    // Construct an error message
    let error_msg = format!(
        "Service {} crashed (job_result: {}, exit_code: {}, exit_status: {}, invocation_id: {})",
        unit, job_result, exit_code, exit_status, invocation_id
    );

    let build_version = read_or_unknown(BUILD_VERSION_PATH);
    let build_date = read_or_unknown(BUILD_DATE_PATH);

    sentry::configure_scope(|scope: &mut Scope| {
        let date = Utc::now();
        let logs_filename = format!("{hostname} {unit} {date} service logs.txt");
        let diagnostics_filename = format!("{hostname} {unit} {date} memory diagnostics.txt");
        let general_journal_filename = format!("{hostname} {unit} {date} general oom journal.txt");

        let service_logs_attachment: Attachment = Attachment {
            buffer: collect_service_logs(&unit),
            filename: logs_filename,
            content_type: Some("text/plain".to_string()),
            ty: Some(AttachmentType::Attachment),
        };

        let diagnostics_attachment: Attachment = Attachment {
            buffer: collect_memory_diagnostics(
                &unit,
                &hostname,
                &build_version,
                &build_date,
                &job_result,
                &exit_code,
                &exit_status,
                &invocation_id,
            ),
            filename: diagnostics_filename,
            content_type: Some("text/plain".to_string()),
            ty: Some(AttachmentType::Attachment),
        };

        let mut map = std::collections::BTreeMap::new();
        map.insert(String::from("unit"), unit.clone().into());
        map.insert(String::from("hostname"), hostname.clone().into());
        map.insert(String::from("job_result"), job_result.clone().into());
        map.insert(String::from("exit_code"), exit_code.clone().into());
        map.insert(String::from("exit_status"), exit_status.clone().into());
        map.insert(String::from("invocation_id"), invocation_id.clone().into());
        scope.set_context("machine", sentry::protocol::Context::Other(map));
        scope.add_attachment(service_logs_attachment);
        scope.add_attachment(diagnostics_attachment);
        if is_oom_failure(&job_result, &exit_status) {
            let general_journal_attachment: Attachment = Attachment {
                buffer: collect_general_journal_logs(&unit),
                filename: general_journal_filename,
                content_type: Some("text/plain".to_string()),
                ty: Some(AttachmentType::Attachment),
            };
            scope.add_attachment(general_journal_attachment);

            if let Some((high_memory_unit, buffer)) = collect_high_memory_service_logs(&unit) {
                let high_memory_logs_filename =
                    format!("{hostname} {high_memory_unit} {date} high memory service logs.txt");
                let high_memory_logs_attachment: Attachment = Attachment {
                    buffer,
                    filename: high_memory_logs_filename,
                    content_type: Some("text/plain".to_string()),
                    ty: Some(AttachmentType::Attachment),
                };
                scope.add_attachment(high_memory_logs_attachment);
                scope.set_tag("high-memory-unit", high_memory_unit);
            }
        }
        scope.set_tag("unit", unit.clone());
        scope.set_tag("job-result", job_result.clone());
        scope.set_tag("exit-code", exit_code.clone());
        scope.set_tag("exit-status", exit_status.clone());
        scope.set_tag("hostname", hostname.clone());
        scope.set_tag("build-version", build_version.trim().to_string());
        scope.set_tag("build-date", build_date.trim().to_string());
    });

    // Send to Sentry
    sentry::capture_message(&error_msg, sentry::Level::Error);

    Hub::with_active(|hub| {
        hub.bind_client(secondary_client);
        hub.capture_message(&error_msg, sentry::Level::Error);
    });

    println!("Captured error: {}", error_msg);
}
