mod doctor;

use clap::{Parser, Subcommand};

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Doctor => doctor::run(&pulse_core::config::Settings::from_env()).await,
    }
}
