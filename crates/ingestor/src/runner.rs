//! Live mode: one polling loop per source, publishing new articles to Kafka.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use metrics::{counter, gauge, histogram};
use pulse_core::config::Settings;
use pulse_core::topics;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::normalize::{Candidate, Normalizer};
use crate::publisher::Publisher;
use crate::seen::{self, SeenSet};
use crate::source::AnySource;
use crate::sources::{Defaults, Kind, SourceConfig, SourcesFile};

const MAX_BACKOFF: Duration = Duration::from_secs(3600);
const PRUNE_EVERY: Duration = Duration::from_secs(600);

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn http_client(defaults: &Defaults) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(&defaults.user_agent)
        .timeout(defaults.timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5))
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .build()?)
}

/// Global and per-host concurrency limits, so we never hammer one publisher.
#[derive(Clone)]
pub struct Limits {
    global: Arc<Semaphore>,
    per_host: Arc<HashMap<String, Arc<Semaphore>>>,
}

impl Limits {
    pub fn new<'a>(sources: impl IntoIterator<Item = &'a SourceConfig>, d: &Defaults) -> Self {
        let per_host = sources
            .into_iter()
            .map(|s| {
                let host = s.host();
                (host, Arc::new(Semaphore::new(d.max_concurrent_per_host)))
            })
            .collect();
        Self {
            global: Arc::new(Semaphore::new(d.max_concurrent)),
            per_host: Arc::new(per_host),
        }
    }

    pub async fn acquire(
        &self,
        host: &str,
    ) -> (OwnedSemaphorePermit, Option<OwnedSemaphorePermit>) {
        let host_permit = match self.per_host.get(host) {
            Some(sem) => Some(sem.clone().acquire_owned().await.expect("never closed")),
            None => None,
        };
        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("never closed");
        (global, host_permit)
    }
}

struct Ctx {
    defaults: Defaults,
    http: reqwest::Client,
    publisher: Publisher,
    seen: Mutex<SeenSet>,
    normalizer: Normalizer,
    limits: Limits,
    shutdown: CancellationToken,
}

pub async fn run(
    file: SourcesFile,
    settings: &Settings,
    shutdown: CancellationToken,
) -> Result<()> {
    let defaults = file.defaults.clone();
    let sources: Vec<SourceConfig> = file.enabled().cloned().collect();
    ensure!(!sources.is_empty(), "no enabled sources");

    // Remember a bit longer than max_age so ids can't expire while still fresh.
    let ttl = defaults.max_age + Duration::from_secs(3600);
    let mut seen = SeenSet::new(ttl);
    let since = now_ms() - ttl.as_millis() as i64;
    let brokers = settings.kafka_brokers.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        seen::load_from_kafka(&brokers, topics::ARTICLES_RAW, since)
    })
    .await??;
    for (id, t) in loaded {
        seen.insert(id, t);
    }
    tracing::info!(
        ids = seen.len(),
        "seen set rebuilt from {}",
        topics::ARTICLES_RAW
    );

    let ctx = Arc::new(Ctx {
        http: http_client(&defaults)?,
        publisher: Publisher::new(&settings.kafka_brokers)?,
        seen: Mutex::new(seen),
        normalizer: Normalizer {
            max_age: Some(defaults.max_age),
            max_summary_chars: defaults.max_summary_chars,
        },
        limits: Limits::new(&sources, &defaults),
        defaults,
        shutdown,
    });

    tracing::info!(sources = sources.len(), "starting pollers");
    let mut tasks = JoinSet::new();
    for cfg in sources {
        tasks.spawn(source_loop(ctx.clone(), cfg));
    }
    tasks.spawn(prune_loop(ctx.clone()));
    while let Some(res) = tasks.join_next().await {
        if let Err(e) = res {
            tracing::error!(error = %e, "poller task panicked");
        }
    }

    ctx.publisher.flush()?;
    tracing::info!("stopped");
    Ok(())
}

async fn source_loop(ctx: Arc<Ctx>, cfg: SourceConfig) {
    let interval = cfg.interval(&ctx.defaults);
    let host = cfg.host();
    let kind = match cfg.kind {
        Kind::Rss => "rss",
        Kind::Gdelt => "gdelt",
    };
    let mut source = AnySource::new(&cfg, &ctx.defaults);
    let mut rng = seed(&cfg.id);
    // Spread first polls across the interval instead of all at startup.
    let mut delay = interval.mul_f64(next_f64(&mut rng));
    let mut failures = 0u32;

    loop {
        tokio::select! {
            _ = ctx.shutdown.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }

        let started = Instant::now();
        let result = {
            let _permits = ctx.limits.acquire(&host).await;
            source.poll(&ctx.http).await
        };
        histogram!("pulse_ingestor_poll_duration_seconds", "kind" => kind)
            .record(started.elapsed().as_secs_f64());

        match result {
            Ok(poll) => {
                failures = 0;
                let outcome = if poll.not_modified {
                    "not_modified"
                } else {
                    "ok"
                };
                counter!("pulse_ingestor_polls_total", "source" => cfg.id.clone(), "outcome" => outcome)
                    .increment(1);
                gauge!("pulse_ingestor_last_success_timestamp_seconds", "source" => cfg.id.clone())
                    .set(now_ms() as f64 / 1000.0);
                let published = ctx.ingest(&cfg, poll.items).await;
                if published > 0 {
                    tracing::info!(source = %cfg.id, published, "new articles");
                }
                delay = interval.mul_f64(0.9 + 0.2 * next_f64(&mut rng));
            }
            Err(e) => {
                failures += 1;
                counter!("pulse_ingestor_polls_total", "source" => cfg.id.clone(), "outcome" => e.label())
                    .increment(1);
                let backoff = interval
                    .saturating_mul(1 << failures.min(5))
                    .min(MAX_BACKOFF)
                    .mul_f64(0.9 + 0.2 * next_f64(&mut rng));
                delay = e.retry_after().map_or(backoff, |ra| ra.max(backoff));
                tracing::warn!(source = %cfg.id, error = %e, failures, retry_in = ?delay, "poll failed");
            }
        }
    }
}

impl Ctx {
    /// Normalizes, dedups and publishes. Returns the number published.
    async fn ingest(&self, cfg: &SourceConfig, items: Vec<Candidate>) -> usize {
        let fetched_at = now_ms();
        let mut published = 0;
        for candidate in items {
            let item = |result: &'static str| counter!("pulse_ingestor_items_total", "source" => cfg.id.clone(), "result" => result);
            let normalized = match self.normalizer.normalize(cfg, candidate, fetched_at) {
                Ok(n) => n,
                Err(rejection) => {
                    item(rejection.label()).increment(1);
                    continue;
                }
            };
            let article = normalized.article;
            if !self
                .seen
                .lock()
                .expect("poisoned")
                .claim(&article.id, fetched_at)
            {
                item("seen").increment(1);
                continue;
            }
            match self.publisher.publish(&article).await {
                Ok(()) => {
                    published += 1;
                    item("published").increment(1);
                    counter!("pulse_ingestor_published_total", "lang" => article.lang.clone())
                        .increment(1);
                    if let Some(c) = normalized.correction {
                        counter!("pulse_ingestor_event_time_corrections_total", "reason" => c.label())
                            .increment(1);
                    }
                }
                Err(e) => {
                    self.seen.lock().expect("poisoned").release(&article.id);
                    item("publish_error").increment(1);
                    tracing::warn!(source = %cfg.id, error = %e, "publish failed");
                }
            }
        }
        gauge!("pulse_ingestor_seen_set_size")
            .set(self.seen.lock().expect("poisoned").len() as f64);
        published
    }
}

async fn prune_loop(ctx: Arc<Ctx>) {
    loop {
        {
            let mut seen = ctx.seen.lock().expect("poisoned");
            seen.prune(now_ms());
            gauge!("pulse_ingestor_seen_set_size").set(seen.len() as f64);
        }
        tokio::select! {
            _ = ctx.shutdown.cancelled() => return,
            _ = tokio::time::sleep(PRUNE_EVERY) => {}
        }
    }
}

/// Per-source xorshift state, seeded from the id so staggering is stable across restarts.
fn seed(id: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    h.finish() | 1
}

fn next_f64(state: &mut u64) -> f64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_in_unit_interval_and_stable() {
        let mut a = seed("bbc-world");
        let mut b = seed("bbc-world");
        for _ in 0..1000 {
            let x = next_f64(&mut a);
            assert!((0.0..1.0).contains(&x));
            assert_eq!(x, next_f64(&mut b));
        }
    }
}
