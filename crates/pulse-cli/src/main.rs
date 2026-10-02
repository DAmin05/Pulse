mod doctor;
mod fixture;
mod topic;

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
    /// Inspect Kafka topics.
    #[command(subcommand)]
    Topic(TopicCommand),
}

#[derive(Subcommand)]
enum TopicCommand {
    /// Count committed records and duplicate keys (exits 1 on duplicates).
    Check { topic: String },
}

#[derive(Subcommand)]
enum FixtureCommand {
    /// Record a topic into a fixture file. articles.raw is ordered by
    /// (fetched_at, id); articles.embedded keeps log order.
    Record {
        #[arg(long, default_value = pulse_core::topics::ARTICLES_RAW)]
        topic: String,
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
        Command::Fixture(FixtureCommand::Record { topic, since, out }) => {
            fixture::record(&settings, &topic, since, &out).await
        }
        Command::Fixture(FixtureCommand::Stats { path }) => fixture::stats(&path),
        Command::Topic(TopicCommand::Check { topic }) => {
            if !topic::check(&settings, &topic).await? {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}
