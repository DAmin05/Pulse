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
    /// Count committed records and duplicate ids (exits 1 on duplicates).
    /// Ids are message keys, or event_id for story event topics.
    Check { topic: String },
    /// Print "<records> <sha256>" over committed records in order.
    Hash { topic: String },
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
    /// Publish a fixture's records into a topic, in order.
    Publish {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        topic: String,
    },
    /// Generate a deterministic synthetic embedded fixture (for tests and CI).
    Synth {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 5000)]
        count: usize,
        #[arg(long, default_value_t = 7)]
        seed: u64,
    },
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
        Command::Fixture(FixtureCommand::Publish { file, topic }) => {
            fixture::publish(&settings, &file, &topic).await
        }
        Command::Fixture(FixtureCommand::Synth { out, count, seed }) => {
            fixture::synth(&out, count, seed)
        }
        Command::Topic(TopicCommand::Hash { topic }) => topic::hash(&settings, &topic).await,
        Command::Topic(TopicCommand::Check { topic }) => {
            if !topic::check(&settings, &topic).await? {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}
