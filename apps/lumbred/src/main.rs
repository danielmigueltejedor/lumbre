//! `lumbred` — the Lumbre daemon.
//!
//! It is small on purpose: it links no inference code (models run in a worker process) and does
//! nothing at all while idle. Started by hand, by the desktop app, or by systemd socket
//! activation, in which case `--exit-on-idle` lets it disappear again when it is not needed.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use lumbre_api::{load_or_create_token, Paths};
use lumbre_core::{Config, Engine, History};

const HELP: &str = "\
lumbred — the Lumbre daemon (local API and model worker supervisor)

Usage: lumbred [OPTIONS]

Options:
      --port <PORT>          Loopback port to listen on (default: `port` in config.toml, 47110)
      --exit-on-idle <SECS>  Exit after this long with no model loaded and no requests
                             (for socket activation)
      --log-json             Write logs as JSON lines instead of text
  -V, --version              Print the version
  -h, --help                 Print this help

Logs go to stderr; set LUMBRE_LOG (e.g. `debug`, `lumbre_core=trace`) to change the level.
Files live under $LUMBRE_HOME when set, otherwise in the per-user directories of the platform.";

#[derive(Debug, Default)]
struct Args {
    port: Option<u16>,
    exit_on_idle: Option<Duration>,
    log_json: bool,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut parsed = Args::default();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("lumbred {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--log-json" => parsed.log_json = true,
            "--port" => {
                parsed.port =
                    Some(value("--port")?.parse().map_err(|_| "--port must be a number".to_owned())?)
            }
            "--exit-on-idle" => {
                let secs: u64 = value("--exit-on-idle")?
                    .parse()
                    .map_err(|_| "--exit-on-idle must be seconds".to_owned())?;
                parsed.exit_on_idle = Some(Duration::from_secs(secs.max(1)));
            }
            other => return Err(format!("unknown argument `{other}` (see --help)")),
        }
    }
    Ok(Some(parsed))
}

fn init_logging(json: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("LUMBRE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr);
    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}

/// The socket systemd passed us (`LISTEN_FDS`), if this process was socket-activated.
#[cfg(unix)]
#[allow(unsafe_code)]
fn activation_socket() -> Option<TcpListener> {
    use std::os::fd::FromRawFd;
    let for_us = std::env::var("LISTEN_PID").ok()?.parse::<u32>().ok()? == std::process::id();
    let count: u32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    if !for_us || count < 1 {
        return None;
    }
    // SAFETY: the sd_listen_fds protocol guarantees that, when LISTEN_PID is our pid, descriptor
    // 3 is an open socket handed to this process and owned by nobody else in it.
    Some(unsafe { TcpListener::from_raw_fd(3) })
}

#[cfg(not(unix))]
fn activation_socket() -> Option<TcpListener> {
    None
}

async fn shutdown_signal(app: lumbred::App, exit_on_idle: Option<Duration>) {
    let idle = async {
        match exit_on_idle {
            Some(quiet) => loop {
                tokio::time::sleep(quiet / 2).await;
                if app.idle_for(quiet) {
                    tracing::info!(secs = quiet.as_secs(), "idle; exiting");
                    break;
                }
            },
            None => std::future::pending().await,
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = idle => {}
        () = terminate => tracing::info!("terminated"),
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupted"),
    }
}

fn run(args: Args) -> Result<(), String> {
    init_logging(args.log_json);
    let paths = Paths::discover();
    let config = Config::load(&paths.config_file()).map_err(|error| error.to_string())?;
    let token = load_or_create_token(&paths.token_file()).map_err(|error| format!("API token: {error}"))?;

    let listener = match activation_socket() {
        Some(listener) => listener,
        None => {
            // Loopback only: the API is never reachable from another machine.
            let address = SocketAddr::from((Ipv4Addr::LOCALHOST, args.port.unwrap_or(config.port)));
            TcpListener::bind(address).map_err(|error| format!("cannot listen on {address}: {error}"))?
        }
    };
    listener.set_nonblocking(true).map_err(|error| error.to_string())?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    if !address.ip().is_loopback() {
        return Err(format!("refusing to serve the API on non-loopback address {address}"));
    }
    std::fs::create_dir_all(&paths.data_dir)
        .and_then(|()| std::fs::write(paths.endpoint_file(), address.to_string()))
        .map_err(|error| format!("cannot write {}: {error}", paths.endpoint_file().display()))?;

    let app = lumbred::shared(Engine::new(config, &paths), History::new(paths.conversations_dir()), token);
    // One thread serves the API; blocking work (generation, downloads) runs on demand-spawned
    // threads that are released again after a short idle time.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .thread_keep_alive(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    let result = runtime.block_on(async {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        tokio::spawn(lumbred::housekeeping(Arc::clone(&app)));
        tracing::info!(%address, version = env!("CARGO_PKG_VERSION"), "listening");
        axum::serve(listener, lumbred::router(Arc::clone(&app)))
            .with_graceful_shutdown(shutdown_signal(Arc::clone(&app), args.exit_on_idle))
            .await
    });
    app.shutdown();
    let _ = std::fs::remove_file(paths.endpoint_file());
    result.map_err(|error| error.to_string())
}

fn main() -> ExitCode {
    let outcome = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => run(args),
        Ok(None) => Ok(()),
        Err(message) => Err(message),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("lumbred: {message}");
            ExitCode::FAILURE
        }
    }
}
