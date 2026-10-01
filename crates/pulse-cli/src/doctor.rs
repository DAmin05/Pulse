use std::time::Duration;

use anyhow::{Context, Result, bail};
use pulse_core::{config::Settings, topics};
use rdkafka::consumer::{BaseConsumer, Consumer};

const TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run(settings: &Settings) -> Result<()> {
    let checks = [
        ("kafka topics", check_kafka(settings).await),
        ("postgres + pgvector", check_postgres(settings).await),
        (
            "object store (s3)",
            check_http(&settings.s3_endpoint, "/status").await,
        ),
        (
            "prometheus",
            check_http(&settings.prometheus_url, "/-/ready").await,
        ),
        (
            "grafana",
            check_http(&settings.grafana_url, "/api/health").await,
        ),
    ];

    let mut failed = 0;
    for (name, result) in &checks {
        match result {
            Ok(detail) => println!("  ok    {name:<22} {detail}"),
            Err(e) => {
                failed += 1;
                println!("  FAIL  {name:<22} {e:#}");
            }
        }
    }
    if failed > 0 {
        bail!("{failed} check(s) failed — is the stack up? try `make up`");
    }
    println!("\nall checks passed");
    Ok(())
}

async fn check_kafka(settings: &Settings) -> Result<String> {
    let brokers = settings.kafka_brokers.clone();
    // librdkafka metadata calls block; keep them off the async runtime.
    tokio::task::spawn_blocking(move || {
        let consumer: BaseConsumer = rdkafka::ClientConfig::new()
            .set("bootstrap.servers", &brokers)
            .create()?;
        let metadata = consumer
            .fetch_metadata(None, TIMEOUT)
            .with_context(|| format!("cannot reach brokers at {brokers}"))?;

        let mut problems = Vec::new();
        for spec in topics::REQUIRED {
            match metadata.topics().iter().find(|t| t.name() == spec.name) {
                None => problems.push(format!("{} missing", spec.name)),
                Some(t) if t.partitions().len() as i32 != spec.partitions => {
                    problems.push(format!(
                        "{} has {} partitions, expected {}",
                        spec.name,
                        t.partitions().len(),
                        spec.partitions
                    ))
                }
                Some(_) => {}
            }
        }
        if !problems.is_empty() {
            bail!(problems.join("; "));
        }
        Ok(format!(
            "{} brokers, {} required topics present",
            metadata.brokers().len(),
            topics::REQUIRED.len()
        ))
    })
    .await?
}

async fn check_postgres(settings: &Settings) -> Result<String> {
    let (client, connection) = tokio::time::timeout(
        TIMEOUT,
        tokio_postgres::connect(&settings.database_url, tokio_postgres::NoTls),
    )
    .await
    .context("timed out")??;
    tokio::spawn(connection);

    let row = client
        .query_opt(
            "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
            &[],
        )
        .await?
        .context("pgvector extension not installed")?;
    let version: String = row.get(0);
    Ok(format!("pgvector {version}"))
}

async fn check_http(base: &str, path: &str) -> Result<String> {
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let status = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("cannot reach {url}"))?
        .status();
    if !status.is_success() {
        bail!("{url} returned {status}");
    }
    Ok(url)
}
