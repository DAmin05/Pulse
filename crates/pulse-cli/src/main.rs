mod doctor;
mod fixture;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use pulse_core::config::Settings;

#[derive(Parser)]
#[command(name = "pulse", about = "Pulse developer tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that the local stack is up and correctly configured.
    Doctor,
    /// Golden fixture files.
    #[command(subcommand)]
    Fixture(FixtureCommand),
}

#[derive(Subcommand)]
enum FixtureCommand {
    /// Record articles.raw into a fixture file, ordered by (fetched_at, id).
    Record {
        /// How far back to read, e.g. 24h, 90m.
        #[arg(long, default_value = "24h", value_parser = humantime::parse_duration)]
        since: std::time::Duration,
        #[arg(long)]
        out: PathBuf,
    },
    /// Summarize a fixture file.
    Stats { path: PathBuf },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = Settings::from_env();
    match Cli::parse().command {
        Command::Doctor => doctor::run(&settings).await,
        Command::Fixture(FixtureCommand::Record { since, out }) => {
            fixture::record(&settings, since, &out).await
        }
        Command::Fixture(FixtureCommand::Stats { path }) => fixture::stats(&path),
    }
}
