use sentry::Scope;
use sentry::protocol::Event;
use std::env;
use std::fs;
use std::process::Command;
use std::sync::Arc;

mod system_metrics;

const BUILD_VERSION_PATH: &str = "/opt/image-build-version";
const BUILD_CHANNEL_PATH: &str = "/opt/image-build-channel";
const BUILD_DATE_PATH: &str = "/opt/ROOTFS_BUILD_DATE";
const MACHINE_CONFIG_PATH: &str = "/meticulous-user/config/config.yml";
const UNKNOWN: &str = "unknown";
const MAX_TAG_LENGTH: usize = 128;
const MICROSECONDS_PER_SECOND: u64 = 1_000_000;
const ALLOWED_EVENT_TAGS: [&str; 12] = [
    "unit",
    "job-result",
    "exit-code",
    "exit-status",
    "build-version",
    "build-channel",
    "build-date",
    "component-version",
    "restart-count",
    "runtime-seconds",
    "crash-reporter-version",
    "serial",
];

fn read_or_unknown(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|cause| {
        println!("Error {cause} while reading file at: {path}");
        UNKNOWN.to_string()
    })
}

fn sanitize_technical_value(value: String) -> String {
    let sanitized: String = value
        .trim()
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | '-' | '@' | ':' | '+' | '~')
        })
        .take(MAX_TAG_LENGTH)
        .collect();

    if sanitized.is_empty() {
        UNKNOWN.to_string()
    } else {
        sanitized
    }
}

fn serial_from_config(config: &str) -> Option<String> {
    let config: serde_yaml::Value = serde_yaml::from_str(config).ok()?;
    let serial = config.get("system")?.get("serial")?.as_str()?;
    let serial = sanitize_technical_value(serial.to_string());

    (serial != UNKNOWN).then_some(serial)
}

fn machine_serial() -> Option<String> {
    let config = fs::read_to_string(MACHINE_CONFIG_PATH).ok()?;
    serial_from_config(&config)
}

fn sanitize_event(mut event: Event<'static>) -> Option<Event<'static>> {
    event.server_name = None;
    event.user = None;
    event.request = None;
    event.culprit = None;
    event.transaction = None;
    event.logentry = None;
    event.logger = None;
    event.modules.clear();
    event.contexts.retain(|key, context| {
        key == system_metrics::CONTEXT_NAME && system_metrics::sanitize(context)
    });
    event.breadcrumbs.values.clear();
    event.exception.values.clear();
    event.stacktrace = None;
    event.template = None;
    event.threads.values.clear();
    event.extra.clear();
    event.debug_meta = Default::default();

    event
        .tags
        .retain(|key, _| ALLOWED_EVENT_TAGS.contains(&key.as_str()));
    for value in event.tags.values_mut() {
        *value = sanitize_technical_value(std::mem::take(value));
    }

    Some(event)
}

fn command_value(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = sanitize_technical_value(String::from_utf8(output.stdout).ok()?);
    (value != UNKNOWN).then_some(value)
}

fn systemd_property(unit: &str, property: &str) -> Option<String> {
    command_value(
        "systemctl",
        &["show", unit, "--property", property, "--value"],
    )
}

fn component_package(unit: &str) -> Option<&'static str> {
    match unit {
        "meticulous-backend.service" => Some("meticulous-backend"),
        "meticulous-dial.service" => Some("meticulous-dial"),
        "meticulous-watcher.service" => Some("meticulous-watcher"),
        "rauc-hawkbit-updater.service" => Some("rauc-hawkbit-updater"),
        _ => None,
    }
}

fn component_version(unit: &str) -> Option<String> {
    let package = component_package(unit)?;
    command_value(
        "dpkg-query",
        &["--show", "--showformat=${Version}", package],
    )
}

fn runtime_seconds(start_micros: &str, exit_micros: &str) -> Option<String> {
    let start_micros = start_micros.parse::<u64>().ok()?;
    let exit_micros = exit_micros.parse::<u64>().ok()?;
    let elapsed_micros = exit_micros.checked_sub(start_micros)?;

    Some((elapsed_micros / MICROSECONDS_PER_SECOND).to_string())
}

fn service_runtime_seconds(unit: &str) -> Option<String> {
    let start = systemd_property(unit, "ExecMainStartTimestampMonotonic")?;
    let exit = systemd_property(unit, "ExecMainExitTimestampMonotonic")?;
    runtime_seconds(&start, &exit)
}

fn crash_message(unit: &str, job_result: &str, exit_code: &str, exit_status: &str) -> String {
    format!(
        "Service {unit} crashed (job_result: {job_result}, exit_code: {exit_code}, exit_status: {exit_status})"
    )
}

fn main() {
    let build_version = sanitize_technical_value(read_or_unknown(BUILD_VERSION_PATH));
    let build_channel = sanitize_technical_value(read_or_unknown(BUILD_CHANNEL_PATH));
    let build_date = sanitize_technical_value(read_or_unknown(BUILD_DATE_PATH));
    let release = format!("meticulous-linux@{build_version}");

    let _guard = sentry::init((
        "https://6f9403694443a82c06c958a8e6f40748@sentry.meticulousespresso.com/4",
        sentry::ClientOptions {
            release: Some(release.into()),
            environment: Some(build_channel.clone().into()),
            send_default_pii: false,
            default_integrations: false,
            max_breadcrumbs: 0,
            auto_session_tracking: false,
            before_breadcrumb: Some(Arc::new(|_| None)),
            before_send: Some(Arc::new(sanitize_event)),
            ..Default::default()
        },
    ));

    let unit =
        sanitize_technical_value(env::var("MONITOR_UNIT").unwrap_or_else(|_| UNKNOWN.to_string()));
    let job_result = sanitize_technical_value(
        env::var("MONITOR_SERVICE_RESULT").unwrap_or_else(|_| UNKNOWN.to_string()),
    );
    let exit_code = sanitize_technical_value(
        env::var("MONITOR_EXIT_CODE").unwrap_or_else(|_| UNKNOWN.to_string()),
    );
    let exit_status = sanitize_technical_value(
        env::var("MONITOR_EXIT_STATUS").unwrap_or_else(|_| UNKNOWN.to_string()),
    );
    let restart_count = systemd_property(&unit, "NRestarts");
    let runtime_seconds = service_runtime_seconds(&unit);
    let component_version = component_version(&unit);
    let serial = machine_serial();
    let system_metrics = system_metrics::collect(&unit);

    sentry::configure_scope(|scope: &mut Scope| {
        scope.clear_breadcrumbs();
        scope.set_tag("unit", unit.clone());
        scope.set_tag("job-result", job_result.clone());
        scope.set_tag("exit-code", exit_code.clone());
        scope.set_tag("exit-status", exit_status.clone());
        scope.set_tag("build-version", build_version.clone());
        scope.set_tag("build-channel", build_channel.clone());
        scope.set_tag("build-date", build_date.clone());
        scope.set_tag("crash-reporter-version", env!("CARGO_PKG_VERSION"));

        if let Some(value) = &component_version {
            scope.set_tag("component-version", value);
        }
        if let Some(value) = &restart_count {
            scope.set_tag("restart-count", value);
        }
        if let Some(value) = &runtime_seconds {
            scope.set_tag("runtime-seconds", value);
        }
        if let Some(value) = &serial {
            scope.set_tag("serial", value);
        }
        scope.set_context(system_metrics::CONTEXT_NAME, system_metrics);

        let fingerprint = [
            "systemd-service-failure",
            unit.as_str(),
            job_result.as_str(),
            exit_code.as_str(),
            exit_status.as_str(),
        ];
        scope.set_fingerprint(Some(&fingerprint));
    });

    let error_message = crash_message(&unit, &job_result, &exit_code, &exit_status);
    sentry::capture_message(&error_message, sentry::Level::Error);
    println!("Captured error: {error_message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn technical_values_are_bounded_and_remove_unapproved_characters() {
        let value = format!(
            " service name\x00 with spaces / secret={} ",
            "x".repeat(200)
        );
        let sanitized = sanitize_technical_value(value);

        assert!(sanitized.starts_with("servicenamewithspacessecret"));
        assert!(sanitized.len() <= MAX_TAG_LENGTH);
        for forbidden in [' ', '/', '=', '\0'] {
            assert!(!sanitized.contains(forbidden));
        }
    }

    #[test]
    fn only_known_services_map_to_component_packages() {
        assert_eq!(
            component_package("meticulous-dial.service"),
            Some("meticulous-dial")
        );
        assert_eq!(
            component_package("meticulous-backend.service"),
            Some("meticulous-backend")
        );
        assert_eq!(component_package("customer-provided.service"), None);
    }

    #[test]
    fn event_sanitizer_keeps_only_approved_diagnostic_tags() {
        let mut event = Event {
            server_name: Some("personal-hostname".into()),
            ..Default::default()
        };
        event
            .tags
            .insert("unit".to_string(), "meticulous-dial.service".to_string());
        event
            .tags
            .insert("hostname".to_string(), "personal-hostname".to_string());

        let sanitized = sanitize_event(event).expect("diagnostic event should be retained");

        assert_eq!(sanitized.server_name, None);
        assert_eq!(
            sanitized.tags.get("unit"),
            Some(&"meticulous-dial.service".to_string())
        );
        assert!(!sanitized.tags.contains_key("hostname"));
    }

    #[test]
    fn serial_is_read_without_exposing_other_config_values() {
        let config = r#"
system:
  serial: "M123-ABC"
wifi:
  KnownWifis:
    - ssid: "Private Network"
      password: "not-for-sentry"
"#;

        assert_eq!(serial_from_config(config), Some("M123-ABC".to_string()));
    }

    #[test]
    fn serial_is_omitted_when_missing_invalid_or_not_a_string() {
        assert_eq!(serial_from_config("system:\n  color: black\n"), None);
        assert_eq!(serial_from_config("system: [invalid"), None);
        assert_eq!(serial_from_config("system:\n  serial: 123\n"), None);
        assert_eq!(serial_from_config("system:\n  serial: ' / = '\n"), None);
    }

    #[test]
    fn event_sanitizer_retains_the_approved_serial_tag() {
        let mut event = Event::default();
        event
            .tags
            .insert("serial".to_string(), "M123-ABC".to_string());

        let sanitized = sanitize_event(event).expect("diagnostic event should be retained");

        assert_eq!(sanitized.tags.get("serial"), Some(&"M123-ABC".to_string()));
    }

    #[test]
    fn event_sanitizer_retains_only_the_approved_system_metrics_context() {
        let mut event = Event::default();
        let mut metrics = std::collections::BTreeMap::new();
        metrics.insert("memory-total-bytes".to_string(), 1024_u64.into());
        metrics.insert("command-line".to_string(), "private argument".into());
        event.contexts.insert(
            system_metrics::CONTEXT_NAME.to_string(),
            sentry::protocol::Context::Other(metrics),
        );
        event.contexts.insert(
            "device".to_string(),
            sentry::protocol::Context::Other(Default::default()),
        );

        let sanitized = sanitize_event(event).expect("diagnostic event should be retained");
        assert_eq!(sanitized.contexts.len(), 1);
        let sentry::protocol::Context::Other(metrics) = sanitized
            .contexts
            .get(system_metrics::CONTEXT_NAME)
            .expect("approved metrics context should remain")
        else {
            panic!("system metrics should remain an arbitrary context");
        };
        assert!(metrics.contains_key("memory-total-bytes"));
        assert!(!metrics.contains_key("command-line"));
    }

    #[test]
    fn runtime_is_derived_from_monotonic_systemd_timestamps() {
        assert_eq!(runtime_seconds("1000000", "4500000"), Some("3".to_string()));
        assert_eq!(runtime_seconds("4500000", "1000000"), None);
        assert_eq!(runtime_seconds("not-a-timestamp", "4500000"), None);
    }

    #[test]
    fn crash_message_contains_only_the_approved_fields() {
        let message = crash_message("meticulous-backend.service", "failed", "exited", "1");

        assert_eq!(
            message,
            "Service meticulous-backend.service crashed (job_result: failed, exit_code: exited, exit_status: 1)"
        );
        assert!(!message.contains("hostname"));
        assert!(!message.contains("invocation"));
    }
}
