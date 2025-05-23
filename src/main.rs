use std::env;

fn main() {
    let _guard = sentry::init((
        "https://295725e5bbfc9b3eb0413cafc1f6cea6@o4506723336060928.ingest.us.sentry.io/4509197049593856",
        sentry::ClientOptions {
            release: sentry::release_name!(),
            send_default_pii: false,
            ..Default::default()
        },
    ));

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
    sentry::configure_scope(|scope| {
        let mut map = std::collections::BTreeMap::new();
        map.insert(String::from("unit"), unit.clone().into());
        map.insert(String::from("hostname"), hostname.clone().into());
        scope.set_context("machine", sentry::protocol::Context::Other(map));
    });

    // Send to Sentry
    sentry::capture_message(&error_msg, sentry::Level::Error);
    println!("Captured error: {}", error_msg);
}
