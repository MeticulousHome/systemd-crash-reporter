use sentry::protocol::Context;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub const CONTEXT_NAME: &str = "system-metrics";

const PROC_ROOT: &str = "/proc";
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const ROOT_DISK_PATH: &str = "/";
const USER_DATA_DISK_PATH: &str = "/meticulous-user";
const TOP_PROCESS_COUNT: usize = 3;
const BYTES_PER_KIB: u64 = 1024;

const ALLOWED_CONTEXT_FIELDS: [&str; 18] = [
    "memory-total-bytes",
    "memory-available-bytes",
    "swap-total-bytes",
    "swap-used-bytes",
    "root-disk-total-bytes",
    "root-disk-available-bytes",
    "root-disk-used-percent",
    "user-data-disk-total-bytes",
    "user-data-disk-available-bytes",
    "user-data-disk-used-percent",
    "failed-service-memory-peak-bytes",
    "failed-service-cpu-usage-nsec",
    "top-process-1",
    "top-process-1-rss-bytes",
    "top-process-2",
    "top-process-2-rss-bytes",
    "top-process-3",
    "top-process-3-rss-bytes",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessMemory {
    name: String,
    rss_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemorySnapshot {
    total_bytes: u64,
    available_bytes: u64,
    swap_total_bytes: u64,
    swap_used_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiskSnapshot {
    total_bytes: u64,
    available_bytes: u64,
    used_percent: u64,
}

fn sanitize_process_name(value: &str) -> Option<String> {
    let sanitized: String = value
        .trim()
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | '-' | '@' | ':' | '+' | '~')
        })
        .take(64)
        .collect();

    (!sanitized.is_empty()).then_some(sanitized)
}

fn parse_kib_value(line: &str) -> Option<u64> {
    line.split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(BYTES_PER_KIB)
}

fn parse_meminfo(contents: &str) -> Option<MemorySnapshot> {
    let mut total_bytes = None;
    let mut available_bytes = None;
    let mut swap_total_bytes = None;
    let mut swap_free_bytes = None;

    for line in contents.lines() {
        if line.starts_with("MemTotal:") {
            total_bytes = parse_kib_value(line);
        } else if line.starts_with("MemAvailable:") {
            available_bytes = parse_kib_value(line);
        } else if line.starts_with("SwapTotal:") {
            swap_total_bytes = parse_kib_value(line);
        } else if line.starts_with("SwapFree:") {
            swap_free_bytes = parse_kib_value(line);
        }
    }

    let swap_total_bytes = swap_total_bytes?;
    Some(MemorySnapshot {
        total_bytes: total_bytes?,
        available_bytes: available_bytes?,
        swap_total_bytes,
        swap_used_bytes: swap_total_bytes.checked_sub(swap_free_bytes?)?,
    })
}

fn memory_snapshot() -> Option<MemorySnapshot> {
    parse_meminfo(&fs::read_to_string(Path::new(PROC_ROOT).join("meminfo")).ok()?)
}

fn parse_df_output(output: &str) -> Option<DiskSnapshot> {
    let values = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .last()?;
    let mut values = values.split_whitespace();

    Some(DiskSnapshot {
        total_bytes: values.next()?.parse().ok()?,
        available_bytes: values.next()?.parse().ok()?,
        used_percent: values.next()?.trim_end_matches('%').parse().ok()?,
    })
}

fn disk_snapshot(path: &str) -> Option<DiskSnapshot> {
    let output = Command::new("df")
        .args(["-B1", "--output=size,avail,pcent", "--", path])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_df_output(&String::from_utf8(output.stdout).ok()?)
}

fn parse_process_rss(status: &str) -> Option<u64> {
    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .and_then(parse_kib_value)
}

fn select_top_processes(processes: impl IntoIterator<Item = ProcessMemory>) -> Vec<ProcessMemory> {
    let mut processes: Vec<ProcessMemory> = processes.into_iter().collect();
    processes.sort_by(|left, right| {
        right
            .rss_bytes
            .cmp(&left.rss_bytes)
            .then_with(|| left.name.cmp(&right.name))
    });
    processes.truncate(TOP_PROCESS_COUNT);
    processes
}

fn top_processes() -> Vec<ProcessMemory> {
    let Ok(entries) = fs::read_dir(PROC_ROOT) else {
        return Vec::new();
    };

    let processes = entries.filter_map(|entry| {
        let entry = entry.ok()?;
        let pid = entry.file_name();
        pid.to_str()?.parse::<u32>().ok()?;

        let process_dir = entry.path();
        let name = sanitize_process_name(&fs::read_to_string(process_dir.join("comm")).ok()?)?;
        let rss_bytes = parse_process_rss(&fs::read_to_string(process_dir.join("status")).ok()?)?;
        Some(ProcessMemory { name, rss_bytes })
    });

    select_top_processes(processes)
}

fn safe_cgroup_path(control_group: &str) -> Option<PathBuf> {
    let relative = Path::new(control_group.trim()).strip_prefix("/").ok()?;
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }

    Some(Path::new(CGROUP_ROOT).join(relative))
}

fn parse_unsigned(value: &str) -> Option<u64> {
    value.trim().parse().ok()
}

fn failed_service_memory_peak(control_group: Option<&str>) -> Option<u64> {
    let path = safe_cgroup_path(control_group?)?.join("memory.peak");
    parse_unsigned(&fs::read_to_string(path).ok()?)
}

fn command_value(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn systemd_property(unit: &str, property: &str) -> Option<String> {
    command_value(
        "systemctl",
        &["show", unit, "--property", property, "--value"],
    )
}

fn insert_disk_metrics(
    map: &mut BTreeMap<String, sentry::protocol::Value>,
    prefix: &str,
    snapshot: DiskSnapshot,
) {
    map.insert(
        format!("{prefix}-disk-total-bytes"),
        snapshot.total_bytes.into(),
    );
    map.insert(
        format!("{prefix}-disk-available-bytes"),
        snapshot.available_bytes.into(),
    );
    map.insert(
        format!("{prefix}-disk-used-percent"),
        snapshot.used_percent.into(),
    );
}

pub fn collect(unit: &str) -> Context {
    let mut map = BTreeMap::new();

    if let Some(memory) = memory_snapshot() {
        map.insert("memory-total-bytes".to_string(), memory.total_bytes.into());
        map.insert(
            "memory-available-bytes".to_string(),
            memory.available_bytes.into(),
        );
        map.insert(
            "swap-total-bytes".to_string(),
            memory.swap_total_bytes.into(),
        );
        map.insert("swap-used-bytes".to_string(), memory.swap_used_bytes.into());
    }

    if let Some(disk) = disk_snapshot(ROOT_DISK_PATH) {
        insert_disk_metrics(&mut map, "root", disk);
    }
    if let Some(disk) = disk_snapshot(USER_DATA_DISK_PATH) {
        insert_disk_metrics(&mut map, "user-data", disk);
    }

    let control_group = systemd_property(unit, "ControlGroup");
    if let Some(memory_peak) = failed_service_memory_peak(control_group.as_deref()) {
        map.insert(
            "failed-service-memory-peak-bytes".to_string(),
            memory_peak.into(),
        );
    }
    if let Some(cpu_usage) =
        systemd_property(unit, "CPUUsageNSec").and_then(|value| parse_unsigned(&value))
    {
        map.insert(
            "failed-service-cpu-usage-nsec".to_string(),
            cpu_usage.into(),
        );
    }

    for (index, process) in top_processes().into_iter().enumerate() {
        let position = index + 1;
        map.insert(format!("top-process-{position}"), process.name.into());
        map.insert(
            format!("top-process-{position}-rss-bytes"),
            process.rss_bytes.into(),
        );
    }

    Context::Other(map)
}

pub fn sanitize(context: &mut Context) -> bool {
    let Context::Other(values) = context else {
        return false;
    };

    values.retain(|key, _| ALLOWED_CONTEXT_FIELDS.contains(&key.as_str()));
    !values.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_is_reduced_to_approved_numeric_metrics() {
        let contents = "\
MemTotal:        4096000 kB
MemFree:          100000 kB
MemAvailable:     512000 kB
Buffers:           64000 kB
SwapTotal:       1024000 kB
SwapFree:         768000 kB
";

        assert_eq!(
            parse_meminfo(contents),
            Some(MemorySnapshot {
                total_bytes: 4_194_304_000,
                available_bytes: 524_288_000,
                swap_total_bytes: 1_048_576_000,
                swap_used_bytes: 262_144_000,
            })
        );
    }

    #[test]
    fn df_output_is_reduced_to_capacity_values() {
        let output = "\
1B-blocks       Avail Use%
31138512896 11800000000  63%
";

        assert_eq!(
            parse_df_output(output),
            Some(DiskSnapshot {
                total_bytes: 31_138_512_896,
                available_bytes: 11_800_000_000,
                used_percent: 63,
            })
        );
    }

    #[test]
    fn process_rss_parser_ignores_other_process_status_fields() {
        let status = "\
Name:\tmeticulous-dial
State:\tS (sleeping)
VmSize:\t1200000 kB
VmRSS:\t812000 kB
";

        assert_eq!(parse_process_rss(status), Some(831_488_000));
    }

    #[test]
    fn process_names_are_bounded_and_remove_unapproved_characters() {
        assert_eq!(
            sanitize_process_name(" WebKit Web/Process --customer=value "),
            Some("WebKitWebProcess--customervalue".to_string())
        );
        assert_eq!(sanitize_process_name(" / = "), None);
    }

    #[test]
    fn top_processes_are_sorted_and_limited() {
        let selected = select_top_processes([
            ProcessMemory {
                name: "watcher".to_string(),
                rss_bytes: 90,
            },
            ProcessMemory {
                name: "dial".to_string(),
                rss_bytes: 800,
            },
            ProcessMemory {
                name: "backend".to_string(),
                rss_bytes: 300,
            },
            ProcessMemory {
                name: "journald".to_string(),
                rss_bytes: 100,
            },
        ]);

        assert_eq!(
            selected,
            vec![
                ProcessMemory {
                    name: "dial".to_string(),
                    rss_bytes: 800,
                },
                ProcessMemory {
                    name: "backend".to_string(),
                    rss_bytes: 300,
                },
                ProcessMemory {
                    name: "journald".to_string(),
                    rss_bytes: 100,
                },
            ]
        );
    }

    #[test]
    fn cgroup_path_cannot_escape_the_cgroup_root() {
        assert_eq!(
            safe_cgroup_path("/system.slice/meticulous-backend.service"),
            Some(PathBuf::from(
                "/sys/fs/cgroup/system.slice/meticulous-backend.service"
            ))
        );
        assert_eq!(safe_cgroup_path("/../../meticulous-user"), None);
        assert_eq!(safe_cgroup_path("/"), None);
    }

    #[test]
    fn context_sanitizer_keeps_only_approved_metrics() {
        let mut values = BTreeMap::new();
        values.insert("memory-total-bytes".to_string(), 1024_u64.into());
        values.insert("command-line".to_string(), "private argument".into());
        let mut context = Context::Other(values);

        assert!(sanitize(&mut context));
        let Context::Other(values) = context else {
            panic!("approved context should remain an arbitrary context");
        };
        assert!(values.contains_key("memory-total-bytes"));
        assert!(!values.contains_key("command-line"));
    }
}
