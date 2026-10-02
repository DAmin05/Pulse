//! Logging setup. `RUST_LOG` controls filtering (default `info`);
//! `PULSE_LOG_FORMAT=json` switches to structured output.

use tracing_subscriber::{EnvFilter, fmt};

pub fn init(service: &'static str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Colors only for a terminal; log files and pipes get plain text.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let builder = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(ansi);
    if std::env::var("PULSE_LOG_FORMAT").is_ok_and(|v| v == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
    tracing::info!(service, version = env!("CARGO_PKG_VERSION"), "starting");
}
