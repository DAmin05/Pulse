//! Polls RSS/API sources and publishes raw articles to Kafka.

mod backfill;
mod check;
mod gdelt;
mod lang;
mod normalize;
mod publisher;
mod rss;
mod runner;
mod seen;
mod source;
mod sources;
mod telemetry;
mod text;

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use pulse_core::config::{Settings, env_or};
use tokio_util::sync::CancellationToken;

use sources::{GdeltStream, Kind, SourceConfig, SourcesFile};

#[derive(Parser)]
#[command(name = "ingestor", about = "Pulse ingestor")]
struct Cli {
    /// Source configuration.
    #[arg(long, env = "PULSE_SOURCES", default_value = "config/sources.toml")]
    sources: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Poll all enabled sources and publish to Kafka (default).
    Run,
    /// Poll each source once and report health. Publishes nothing.
    Check {
        /// Only these source ids (repeatable).
        #[arg(long = "source")]
        only: Vec<String>,
        /// Include sources with `enabled = false`.
        #[arg(long)]
        include_disabled: bool,
    },
    /// Download a past range of GDELT GKG files into a fixture file.
    BackfillGdelt {
        #[arg(long, value_enum)]
        stream: StreamArg,
        /// Start (RFC 3339, e.g. 2026-10-01T00:00:00Z), inclusive.
        #[arg(long)]
        from: DateTime<Utc>,
        /// End (RFC 3339), exclusive.
        #[arg(long)]
        to: DateTime<Utc>,
        #[arg(long)]
        out: PathBuf,
        /// Fraction of articles to keep (deterministic by URL).
        #[arg(long, default_value_t = 1.0)]
        sample: f64,
        /// Keep only these domains (repeatable).
        #[arg(long = "domain")]
        domains: Vec<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum StreamArg {
    English,
    Translingual,
}

#[tokio::main]
async fn main() -> Result<()> {
    pulse_core::telemetry::init("ingestor");
    let cli = Cli::parse();
    let file = SourcesFile::load(&cli.sources)?;

    match cli.command.unwrap_or(Command::Run) {
        Command::Run => {
            let port = env_or("PULSE_INGESTOR_METRICS_PORT", "9101").parse()?;
            telemetry::install(port)?;
            let shutdown = CancellationToken::new();
            let on_signal = shutdown.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    tracing::info!("shutting down");
                    on_signal.cancel();
                }
            });
            runner::run(file, &Settings::from_env(), shutdown).await
        }
        Command::Check {
            only,
            include_disabled,
        } => {
            if !check::run(&file, &only, include_disabled).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::BackfillGdelt {
            stream,
            from,
            to,
            out,
            sample,
            domains,
        } => {
            let stream = match stream {
                StreamArg::English => GdeltStream::English,
                StreamArg::Translingual => GdeltStream::Translingual,
            };
            let id = match stream {
                GdeltStream::English => "gdelt-english",
                GdeltStream::Translingual => "gdelt-translingual",
            };
            let cfg = SourceConfig {
                id: id.into(),
                kind: Kind::Gdelt,
                enabled: true,
                url: None,
                lang: None,
                detect_lang: false,
                category: None,
                region: None,
                poll_interval: None,
                stream: Some(stream),
                sample,
                domains,
            };
            backfill::run(&cfg, &file.defaults, from, to, &out).await
        }
    }
}
