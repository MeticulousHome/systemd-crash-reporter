use std::env;
use sentry::{protocol::{Attachment, AttachmentType}, types::Dsn, Client, Hub, Scope};
use std::process::Command;
use chrono::Utc;
use std::sync::Arc;

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

    // Get 5 mins worth of logs from the crashed service, buffer it  
    let output = Command::new("sh")
                                                        .arg("-c")
                                                        .arg(format!("journalctl --no-pager -u {} --since=\"5 minutes ago\"", unit))
                                                        .output();
    const BUILD_VERSION_PATH : &str = "/opt/image-build-version";
    const BUILD_DATE_PATH : &str = "/opt/ROOTFS_BUILD_DATE";
    let mut build_version = String::from("unknown");
    if std::path::Path::new(BUILD_VERSION_PATH).exists(){
        let output = Command::new("sh")
                                        .arg("-c")
                                        .arg(format!("cat {} ", BUILD_VERSION_PATH))
                                        .output();
        build_version = match output {
            Ok(out) => {
                let b_v = String::from_utf8_lossy(&out.stdout).into_owned();
                println!("Got build version: {b_v}\n",);
                b_v
            }

            Err(e) =>{
                eprintln!("Command failed: {}", e);
                String::from("unknown")
            }
        }
    }
    let mut build_date = String::from("unknown");
    if std::path::Path::new(BUILD_DATE_PATH).exists(){
        let output = Command::new("sh")
                                                            .arg("-c")
                                                            .arg(format!("cat {} ", BUILD_DATE_PATH))
                                                            .output();
        build_date = match output {
            Ok(out) => {
                let b_d = String::from_utf8_lossy(&out.stdout).into_owned();
                println!("Got build date: {b_d}\n",);
                b_d
            }

            Err(e) =>{
                eprintln!("Command failed: {}", e);
                String::from("unknown")
            }
        }
    }
    sentry::configure_scope(|scope: &mut Scope| {
        let header = format!("\n =============== LAST 5 MINUTES OF LOGS FROM {} ===============\n\n", unit.to_ascii_uppercase()).into_bytes();
        let data: Vec<u8> = match output {
            Ok(cmd_out) => {
                let logs = String::from_utf8_lossy(&cmd_out.stdout).to_ascii_lowercase();
                println!("Got logs from {unit}\n",);

                // Convert the command output to lowercase and append it to a header
                [header.as_slice(), logs.as_bytes()].concat()
            }

            Err(e) => {
                // Log the error or do other side effects
                eprintln!("Command failed: {}", e);

                let error_msg = format!("Error getting logs from [{}]. e: {}", unit, e);
                error_msg.into_bytes()
            }
        };

        let date = Utc::now();
        let filename = format!("{hostname} {unit} {date} crash logs.txt");
        
        let attachment: Attachment = Attachment {
                                                        buffer: data,
                                                        filename: filename,
                                                        content_type: Some("text/plain".to_string()),
                                                        ty: Some(AttachmentType::Attachment) };

        let mut map = std::collections::BTreeMap::new();
        map.insert(String::from("unit"), unit.clone().into());
        map.insert(String::from("hostname"), hostname.clone().into());
        scope.set_context("machine", sentry::protocol::Context::Other(map));
        scope.add_attachment(attachment);
        scope.set_tag("hostname",hostname.clone());
        scope.set_tag("build-version", build_version.clone());
        scope.set_tag("build-date", build_date.clone());
    });

    // Send to Sentry
    sentry::capture_message(&error_msg, sentry::Level::Error);

    Hub::with_active(|hub|{
        hub.bind_client(secondary_client);
        hub.capture_message(&error_msg, sentry::Level::Error);
    });

    println!("Captured error: {}", error_msg);
}
