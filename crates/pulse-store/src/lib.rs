//! Postgres read model for Pulse.
//!
//! - [`migrate`]: versioned schema (embedded SQL files).
//! - [`writer`]: idempotent application of articles and story events, with the
//!   consumed Kafka offsets committed in the same transaction (exactly-once sink).
//! - [`reader`]: queries for the API, including time travel to any log position.

pub mod reader;
pub mod writer;

pub use {deadpool_postgres, pgvector, tokio_postgres};

use anyhow::{Context, Result};
use tokio_postgres::{Client, NoTls};

/// Channel the sink notifies after each committed batch (payload: last story
/// event `input_offset:seq`), so listeners only see durable, queryable events.
pub const EVENTS_CHANNEL: &str = "pulse_events";

const MIGRATIONS: &[(i32, &str)] = &[(1, include_str!("../migrations/001_read_model.sql"))];

/// Connects a single client, driving its connection in the background.
pub async fn connect(url: &str) -> Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("connecting to {}", redact(url)))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!(error = %e, "postgres connection closed");
        }
    });
    Ok(client)
}

/// Connection pool for concurrent readers.
pub fn pool(url: &str, size: usize) -> Result<deadpool_postgres::Pool> {
    let config: tokio_postgres::Config = url.parse().context("invalid database url")?;
    let manager = deadpool_postgres::Manager::new(config, NoTls);
    Ok(deadpool_postgres::Pool::builder(manager)
        .max_size(size)
        .build()?)
}

/// Applies pending migrations, each in its own transaction.
pub async fn migrate(client: &mut Client) -> Result<Vec<i32>> {
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version integer PRIMARY KEY,
                 applied_at timestamptz NOT NULL DEFAULT now())",
        )
        .await?;
    let mut applied = Vec::new();
    for &(version, sql) in MIGRATIONS {
        let tx = client.transaction().await?;
        // Serialize concurrent migrators (e.g. sink and API starting together).
        tx.execute("LOCK TABLE schema_migrations IN EXCLUSIVE MODE", &[])
            .await?;
        let done = tx
            .query_opt(
                "SELECT 1 FROM schema_migrations WHERE version = $1",
                &[&version],
            )
            .await?
            .is_some();
        if !done {
            tx.batch_execute(sql)
                .await
                .with_context(|| format!("migration {version}"))?;
            tx.execute(
                "INSERT INTO schema_migrations (version) VALUES ($1)",
                &[&version],
            )
            .await?;
            applied.push(version);
        }
        tx.commit().await?;
    }
    Ok(applied)
}

fn redact(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(scheme), Some(at)) if at > scheme => {
            format!("{}***{}", &url[..scheme + 3], &url[at..])
        }
        _ => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn redacts_credentials() {
        assert_eq!(
            super::redact("postgres://pulse:secret@localhost:5432/pulse"),
            "postgres://***@localhost:5432/pulse"
        );
        assert_eq!(super::redact("host=localhost"), "host=localhost");
    }
}
