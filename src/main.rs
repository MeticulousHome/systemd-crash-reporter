use sentry::Scope;
use std::env;
use std::fs;
use std::sync::Arc;

const BUILD_VERSION_PATH: &str = "/opt/image-build-version";
const BUILD_DATE_PATH: &str = "/opt/ROOTFS_BUILD_DATE";
const UNKNOWN: &str = "unknown";
const MAX_TAG_LENGTH: usize = 128;

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
                || matches!(character, '.' | '_' | '-' | '@' | ':' | '+')
        })
        .take(MAX_TAG_LENGTH)
        .collect();

    if sanitized.is_empty() {
        UNKNOWN.to_string()
    } else {
        sanitized
    }
}

fn crash_message(unit: &str, job_result: &str, exit_code: &str, exit_status: &str) -> String {
    format!(
        "Service {unit} crashed (job_result: {job_result}, exit_code: {exit_code}, exit_status: {exit_status})"
    )
}

fn main() {
    let _guard = sentry::init((
        "https://7d92061929211477cb44b8071be63441@sentry.meticulousespresso.com/4",
        sentry::ClientOptions {
            release: sentry::release_name!(),
            send_default_pii: false,
            max_breadcrumbs: 0,
            auto_session_tracking: false,
            before_breadcrumb: Some(Arc::new(|_| None)),
            before_send: Some(Arc::new(|mut event| {
                event.server_name = None;
                event.user = None;
                event.request = None;
                event.breadcrumbs.clear();
                Some(event)
            })),
            ..Default::default()
        },
    ));

    let unit = sanitize_technical_value(
        env::var("MONITOR_UNIT").unwrap_or_else(|_| UNKNOWN.to_string()),
    );
    let job_result = sanitize_technical_value(
        env::var("MONITOR_SERVICE_RESULT").unwrap_or_else(|_| UNKNOWN.to_string()),
    );
    let exit_code = sanitize_technical_value(
        env::var("MONITOR_EXIT_CODE").unwrap_or_else(|_| UNKNOWN.to_string()),
    );
    let exit_status = sanitize_technical_value(
        env::var("MONITOR_EXIT_STATUS").unwrap_or_else(|_| UNKNOWN.to_string()),
    );
    let build_version = sanitize_technical_value(read_or_unknown(BUILD_VERSION_PATH));
    let build_date = sanitize_technical_value(read_or_unknown(BUILD_DATE_PATH));

    sentry::configure_scope(|scope: &mut Scope| {
        scope.clear_breadcrumbs();
        scope.set_tag("unit", unit.clone());
        scope.set_tag("job-result", job_result.clone());
        scope.set_tag("exit-code", exit_code.clone());
        scope.set_tag("exit-status", exit_status.clone());
        scope.set_tag("build-version", build_version.clone());
        scope.set_tag("build-date", build_date.clone());
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
        let value = format!(" service name\x00 with spaces / secret={} ", "x".repeat(200));
        let sanitized = sanitize_technical_value(value);

        assert!(sanitized.starts_with("servicenamewithspacessecret"));
        assert!(sanitized.len() <= MAX_TAG_LENGTH);
        for forbidden in [' ', '/', '=', '\0'] {
            assert!(!sanitized.contains(forbidden));
        }
    }

    #[test]
    fn crash_message_contains_only_the_approved_fields() {
        let message = crash_message(
            "meticulous-backend.service",
            "failed",
            "exited",
            "1",
        );

        assert_eq!(
            message,
            "Service meticulous-backend.service crashed (job_result: failed, exit_code: exited, exit_status: 1)"
        );
        assert!(!message.contains("hostname"));
        assert!(!message.contains("invocation"));
    }
}
