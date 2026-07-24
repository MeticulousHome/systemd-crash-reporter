use sentry::Scope;
use std::env;
use std::fs;

const BUILD_VERSION_PATH: &str = "/opt/image-build-version";
const BUILD_DATE_PATH: &str = "/opt/ROOTFS_BUILD_DATE";
const UNKNOWN: &str = "unknown";

fn read_or_unknown(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|cause| {
        println!("Error {cause} while reading file at: {path}");
        UNKNOWN.to_string()
    })
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

    let unit = env::var("MONITOR_UNIT").unwrap_or_else(|_| UNKNOWN.to_string());
    let job_result =
        env::var("MONITOR_SERVICE_RESULT").unwrap_or_else(|_| UNKNOWN.to_string());
    let exit_code = env::var("MONITOR_EXIT_CODE").unwrap_or_else(|_| UNKNOWN.to_string());
    let exit_status =
        env::var("MONITOR_EXIT_STATUS").unwrap_or_else(|_| UNKNOWN.to_string());
    let build_version = read_or_unknown(BUILD_VERSION_PATH);
    let build_date = read_or_unknown(BUILD_DATE_PATH);

    sentry::configure_scope(|scope: &mut Scope| {
        scope.set_tag("unit", unit.clone());
        scope.set_tag("job-result", job_result.clone());
        scope.set_tag("exit-code", exit_code.clone());
        scope.set_tag("exit-status", exit_status.clone());
        scope.set_tag("build-version", build_version.clone());
        scope.set_tag("build-date", build_date.clone());
    });

    let error_message = format!(
        "Service {unit} crashed (job_result: {job_result}, exit_code: {exit_code}, exit_status: {exit_status})"
    );
    sentry::capture_message(&error_message, sentry::Level::Error);
    println!("Captured error: {error_message}");
}
