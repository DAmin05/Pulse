//! Story Processor CLI. Phase 3: offline evaluation on fixture files.
//! Phase 4 adds the live Kafka mode with checkpoints and transactions.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use prost::Message;
use pulse_core::fixture;
use pulse_core::proto::v1::EmbeddedArticle;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use story_processor::ann::{BruteForce, Hnsw, HnswParams, VectorIndex};
use story_processor::centering::Centering;
use story_processor::engine::{Config, Engine};

#[derive(Parser)]
#[command(name = "story-processor", about = "Pulse Story Processor")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Live mode: articles.embedded → stories.events, exactly once (env-configured;
    /// see .env.example).
    Run,
    /// Cluster a fixture and report story quality, throughput and the output hash.
    Eval {
        #[arg(long)]
        fixture: PathBuf,
        #[command(flatten)]
        tuning: Tuning,
        /// Stories to print (ranked by distinct sources).
        #[arg(long, default_value_t = 15)]
        top: usize,
        /// Write a markdown report of every multi-source story here.
        #[arg(long)]
        report: Option<PathBuf>,
        /// List the N joined articles least similar to their story's centroid.
        #[arg(long, default_value_t = 0)]
        audit: usize,
    },
    /// List every split and merge with headlines, and probe the similarity
    /// distributions the thresholds act on.
    Lineage {
        #[arg(long)]
        fixture: PathBuf,
        #[command(flatten)]
        tuning: Tuning,
        #[arg(long)]
        split_max_similarity: Option<f32>,
        #[arg(long)]
        merge_min_similarity: Option<f32>,
        #[arg(long)]
        merge_min_similarity_unanchored: Option<f32>,
        /// Lineage events to print.
        #[arg(long, default_value_t = 20)]
        show: usize,
    },
    /// Fit per-language mean vectors on a fixture and freeze them in a file.
    Calibrate {
        #[arg(long)]
        fixture: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Languages with fewer articles use the global mean.
        #[arg(long, default_value_t = 30)]
        min_samples: usize,
    },
    /// Grid over the similarity thresholds, one summary row per setting.
    Sweep {
        #[arg(long)]
        fixture: PathBuf,
        #[arg(long)]
        centering: Option<PathBuf>,
        #[arg(long, value_delimiter = ',', default_value = "0.84,0.86,0.88,0.90")]
        neighbor: Vec<f32>,
        #[arg(long, value_delimiter = ',', default_value = "0.82,0.84,0.86,0.88")]
        centroid: Vec<f32>,
    },
    /// HNSW recall@k and speed against brute force on the fixture's vectors.
    Recall {
        #[arg(long)]
        fixture: PathBuf,
        #[arg(long, default_value_t = 10)]
        k: usize,
        /// Fraction of vectors held out as queries.
        #[arg(long, default_value_t = 0.1)]
        holdout: f64,
        #[arg(long, value_delimiter = ',', default_value = "16,32,64,128")]
        ef: Vec<usize>,
    },
}

#[derive(Args, Clone)]
struct Tuning {
    /// Per-language centering file from `calibrate`. Defaults to
    /// config/centering/<model version>.json when that exists.
    #[arg(long)]
    centering: Option<PathBuf>,
    /// Cluster raw vectors (for comparison).
    #[arg(long)]
    no_centering: bool,
    #[arg(long)]
    neighbor_similarity: Option<f32>,
    #[arg(long)]
    centroid_similarity: Option<f32>,
    #[arg(long)]
    dup_jaccard: Option<f32>,
    /// Member neighbors needed to join an established story (1 disables).
    #[arg(long)]
    min_votes: Option<usize>,
    /// Cohesion margin; a negative value disables the check.
    #[arg(long, allow_hyphen_values = true)]
    cohesion_margin: Option<f32>,
    /// Disable the watermark (nothing late, no closing): pure clustering quality.
    #[arg(long)]
    no_lateness: bool,
    /// Override the allowed lateness.
    #[arg(long)]
    lateness_hours: Option<f64>,
}

impl Tuning {
    fn config(&self, inputs: &[EmbeddedArticle]) -> Result<Config> {
        let d = Config::default();
        Ok(Config {
            neighbor_similarity: self.neighbor_similarity.unwrap_or(d.neighbor_similarity),
            centroid_similarity: self.centroid_similarity.unwrap_or(d.centroid_similarity),
            dup_jaccard: self.dup_jaccard.unwrap_or(d.dup_jaccard),
            min_votes_established: self.min_votes.unwrap_or(d.min_votes_established),
            allowed_lateness_ms: match (self.no_lateness, self.lateness_hours) {
                (true, _) => None,
                (false, Some(h)) => Some((h * 3_600_000.0) as i64),
                (false, None) => d.allowed_lateness_ms,
            },
            cohesion_margin: match self.cohesion_margin {
                Some(m) if m < 0.0 => None,
                Some(m) => Some(m),
                None => d.cohesion_margin,
            },
            centering: if self.no_centering {
                None
            } else {
                resolve_centering(self.centering.as_deref(), inputs)?
            },
            ..d
        })
    }
}

/// Explicit path, else the conventional file for the fixture's model version.
fn resolve_centering(
    path: Option<&Path>,
    inputs: &[EmbeddedArticle],
) -> Result<Option<Arc<Centering>>> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => match inputs.first() {
            Some(first) => {
                let p = story_processor::centering::default_path(&first.model_version);
                if !p.exists() {
                    tracing::warn!(
                        "no centering file at {}; clustering raw vectors",
                        p.display()
                    );
                    return Ok(None);
                }
                p
            }
            None => return Ok(None),
        },
    };
    Ok(Some(Arc::new(Centering::load(&path)?)))
}

fn main() -> Result<()> {
    pulse_core::telemetry::init("story-processor");
    match Cli::parse().command {
        Command::Run => tokio::runtime::Runtime::new()?.block_on(run_live()),
        Command::Eval {
            fixture,
            tuning,
            top,
            report,
            audit,
        } => {
            let inputs = load(&fixture)?;
            let cfg = tuning.config(&inputs)?;
            eval(&fixture, inputs, cfg, top, report.as_deref(), audit)
        }
        Command::Lineage {
            fixture,
            tuning,
            split_max_similarity,
            merge_min_similarity,
            merge_min_similarity_unanchored,
            show,
        } => {
            let inputs = load(&fixture)?;
            let mut cfg = tuning.config(&inputs)?;
            if let Some(v) = split_max_similarity {
                cfg.split_max_similarity = v;
            }
            if let Some(v) = merge_min_similarity {
                cfg.merge_min_similarity = v;
            }
            if let Some(v) = merge_min_similarity_unanchored {
                cfg.merge_min_similarity_unanchored = v;
            }
            lineage(&inputs, cfg, show)
        }
        Command::Calibrate {
            fixture,
            out,
            min_samples,
        } => {
            let c = Centering::fit(&load(&fixture)?, min_samples)?;
            c.save(&out)?;
            println!(
                "{} samples, {} languages with own mean ({}), model {} → {}",
                c.samples,
                c.per_lang.len(),
                c.per_lang.keys().cloned().collect::<Vec<_>>().join(","),
                c.model_version,
                out.display()
            );
            Ok(())
        }
        Command::Sweep {
            fixture,
            centering,
            neighbor,
            centroid,
        } => {
            let inputs = load(&fixture)?;
            let centering = resolve_centering(centering.as_deref(), &inputs)?;
            sweep(&inputs, centering, &neighbor, &centroid)
        }
        Command::Recall {
            fixture,
            k,
            holdout,
            ef,
        } => recall(&fixture, k, holdout, &ef),
    }
}

async fn run_live() -> Result<()> {
    use pulse_core::config::{Settings, env_or};
    use pulse_core::topics;
    use std::time::Duration;

    let port: u16 = env_or("PULSE_PROCESSOR_METRICS_PORT", "9103").parse()?;
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(std::net::SocketAddr::from(([0, 0, 0, 0], port)))
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Suffix("seconds".into()),
            &[
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ],
        )?
        .install()?;

    let group_id = env_or("PULSE_PROCESSOR_GROUP", "story-processor");
    let cfg = story_processor::live::LiveConfig {
        brokers: Settings::from_env().kafka_brokers,
        input_topic: env_or("PULSE_PROCESSOR_INPUT_TOPIC", topics::ARTICLES_EMBEDDED),
        output_topic: env_or("PULSE_PROCESSOR_OUTPUT_TOPIC", topics::STORIES_EVENTS),
        late_topic: env_or("PULSE_PROCESSOR_LATE_TOPIC", topics::ARTICLES_LATE),
        snapshot_dir: env_or(
            "PULSE_PROCESSOR_SNAPSHOT_DIR",
            &format!("data/checkpoints/{group_id}"),
        )
        .into(),
        snapshot_every_messages: env_or("PULSE_PROCESSOR_SNAPSHOT_EVERY_MESSAGES", "5000")
            .parse()?,
        snapshot_every: Duration::from_secs(
            env_or("PULSE_PROCESSOR_SNAPSHOT_EVERY_SECS", "300").parse()?,
        ),
        keep_snapshots: env_or("PULSE_PROCESSOR_KEEP_SNAPSHOTS", "5").parse()?,
        max_batch: env_or("PULSE_PROCESSOR_MAX_BATCH", "500").parse()?,
        linger: Duration::from_millis(env_or("PULSE_PROCESSOR_LINGER_MS", "200").parse()?),
        group_id,
    };
    let centering_override = std::env::var("PULSE_CENTERING").ok().map(PathBuf::from);
    let config_for: story_processor::live::ConfigFor = Box::new(move |model_version: &str| {
        let path = centering_override
            .unwrap_or_else(|| story_processor::centering::default_path(model_version));
        let centering = if path.exists() {
            Some(Arc::new(Centering::load(&path)?))
        } else {
            tracing::warn!(
                "no centering file at {}; clustering raw vectors",
                path.display()
            );
            None
        };
        Ok(Config {
            centering,
            ..Config::default()
        })
    });

    let shutdown = tokio_util::sync::CancellationToken::new();
    let on_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("shutting down after the current epoch");
            on_signal.cancel();
        }
    });
    story_processor::live::run(cfg, config_for, shutdown).await
}

fn load(path: &Path) -> Result<Vec<EmbeddedArticle>> {
    fixture::read_all(path).with_context(|| format!("reading {}", path.display()))
}

struct RunStats {
    outcomes: BTreeMap<&'static str, usize>,
    events: usize,
    hash: String,
    seconds: f64,
}

fn run(inputs: &[EmbeddedArticle], cfg: Config) -> (Engine, RunStats) {
    let mut engine = Engine::new(cfg);
    let mut outcomes = BTreeMap::new();
    let mut hasher = Sha256::new();
    let mut events = 0;
    let started = Instant::now();
    for (offset, input) in inputs.iter().enumerate() {
        let processed = engine.process(input, offset as i64);
        let label = story_processor::live::outcome_label(processed.outcome);
        if processed.late {
            *outcomes.entry("(late)").or_default() += 1;
        }
        let out = processed.events;
        *outcomes.entry(label).or_default() += 1;
        for e in &out {
            hasher.update(e.encode_to_vec());
        }
        events += out.len();
    }
    let seconds = started.elapsed().as_secs_f64();
    let hash = hex::encode(&hasher.finalize()[..8]);
    (
        engine,
        RunStats {
            outcomes,
            events,
            hash,
            seconds,
        },
    )
}

struct Summary {
    stories: usize,
    singletons: usize,
    multi_source: usize,
    cross_lingual: usize,
    largest: usize,
    /// Share of articles in multi-source stories.
    covered: f64,
}

fn summarize(engine: &Engine) -> Summary {
    let stories = engine.stories().values();
    let n_articles = engine.articles().len().max(1);
    Summary {
        stories: engine.stories().len(),
        singletons: stories.clone().filter(|s| s.members.len() == 1).count(),
        multi_source: stories.clone().filter(|s| s.sources.len() >= 2).count(),
        cross_lingual: stories.clone().filter(|s| s.langs.len() >= 2).count(),
        largest: stories.clone().map(|s| s.members.len()).max().unwrap_or(0),
        covered: stories
            .filter(|s| s.sources.len() >= 2)
            .map(|s| s.members.len())
            .sum::<usize>() as f64
            / n_articles as f64,
    }
}

fn eval(
    path: &Path,
    inputs: Vec<EmbeddedArticle>,
    cfg: Config,
    top: usize,
    report: Option<&Path>,
    audit: usize,
) -> Result<()> {
    println!(
        "config: centering={} neighbor≥{} centroid≥{} min_votes={} cohesion_margin={:?} \
         dup_jaccard≥{} k={} ef_search={}",
        cfg.centering
            .as_ref()
            .map_or("off", |c| c.model_version.as_str()),
        cfg.neighbor_similarity,
        cfg.centroid_similarity,
        cfg.min_votes_established,
        cfg.cohesion_margin,
        cfg.dup_jaccard,
        cfg.k,
        cfg.hnsw.ef_search
    );
    let (engine, stats) = run(&inputs, cfg);
    let s = summarize(&engine);

    println!("\ninputs        {}", inputs.len());
    for (label, n) in &stats.outcomes {
        println!(
            "  {label:<15} {n:>6}  ({:.1}%)",
            100.0 * *n as f64 / inputs.len() as f64
        );
    }
    println!(
        "stories       {}  (singletons {:.0}%, multi-source {}, cross-lingual {}, largest {})",
        s.stories,
        100.0 * s.singletons as f64 / s.stories.max(1) as f64,
        s.multi_source,
        s.cross_lingual,
        s.largest
    );
    println!(
        "coverage      {:.1}% of articles are in multi-source stories",
        100.0 * s.covered
    );
    println!(
        "throughput    {:.0} articles/s ({:.2}s)",
        inputs.len() as f64 / stats.seconds,
        stats.seconds
    );
    println!("events        {}  sha256 {}", stats.events, stats.hash);

    let mut ranked: Vec<_> = engine
        .stories()
        .values()
        .filter(|s| s.sources.len() >= 2)
        .collect();
    ranked.sort_by(|a, b| {
        b.sources
            .len()
            .cmp(&a.sources.len())
            .then(b.members.len().cmp(&a.members.len()))
            .then(a.id.cmp(&b.id))
    });

    let describe = |story: &story_processor::engine::Story, members: usize| {
        let mut text = String::new();
        let headline = &engine.articles()[&story.headline];
        let _ = writeln!(
            text,
            "{} articles · {} sources · {}\n  ★ {}",
            story.members.len(),
            story.sources.len(),
            story.langs.iter().cloned().collect::<Vec<_>>().join(","),
            headline.title
        );
        for &m in story
            .members
            .iter()
            .filter(|&&m| m != story.headline)
            .take(members)
        {
            let a = &engine.articles()[&m];
            let mark = if a.duplicate_of.is_some() {
                "≈"
            } else {
                "·"
            };
            let _ = writeln!(text, "  {mark} [{} {}] {}", a.lang, a.source_id, a.title);
        }
        text
    };

    println!("\ntop {top} stories by distinct sources:");
    for story in ranked.iter().take(top) {
        print!("\n{}", describe(story, 5));
    }

    if audit > 0 {
        // Precision check: the weakest joins are where false merges show up first.
        let mut weakest: Vec<(f32, u32, u32)> = Vec::new();
        for (&story_key, story) in engine.stories() {
            if story.originals.len() < 2 {
                continue;
            }
            let centroid = story.centroid();
            for &m in story.originals.iter().filter(|&&m| m != story_key) {
                if let Some(v) = engine.indexed_vector(m) {
                    weakest.push((story_processor::ann::dot(v, &centroid), m, story.headline));
                }
            }
        }
        weakest.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        println!(
            "
{audit} weakest joins (similarity to story centroid):"
        );
        for (sim, m, headline) in weakest.iter().take(audit) {
            let a = &engine.articles()[m];
            let h = &engine.articles()[headline];
            println!(
                "\n  {sim:.3}  [{} {}] {}\n     story: {}",
                a.lang, a.source_id, a.title, h.title
            );
        }
    }

    if let Some(out) = report {
        let mut md = format!(
            "# Story report\n\nFixture `{}` · {} inputs · {} stories · {} multi-source\n\n",
            path.display(),
            inputs.len(),
            s.stories,
            s.multi_source
        );
        for story in &ranked {
            let _ = write!(md, "```\n{}```\n\n", describe(story, usize::MAX));
        }
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(out, md)?;
        println!("\nreport: {}", out.display());
    }
    Ok(())
}

fn lineage(inputs: &[EmbeddedArticle], cfg: Config, show: usize) -> Result<()> {
    use pulse_core::proto::v1::story_event::Kind;
    use std::collections::HashMap;

    println!(
        "split if halves < {:.2} similar (≥{} members, ≥{} each); merge if centroids ≥ {:.2} with a \
         shared anchor or ≥ {:.2} without (≥{} members); confirm {} checks, every {} inputs, cooldown {}",
        cfg.split_max_similarity,
        cfg.split_min_size,
        cfg.split_min_component,
        cfg.merge_min_similarity,
        cfg.merge_min_similarity_unanchored,
        cfg.merge_min_size,
        cfg.lineage_confirm_checks,
        cfg.lineage_every_inputs,
        cfg.lineage_cooldown_checks
    );
    let mut engine = Engine::new(cfg.clone());
    let mut headline: HashMap<String, String> = HashMap::new();
    let mut size: HashMap<String, u32> = HashMap::new();
    let (mut splits, mut merges, mut shown_splits, mut shown_merges) = (0, 0, 0, 0);
    for (offset, input) in inputs.iter().enumerate() {
        let events = engine.process(input, offset as i64).events;
        for e in &events {
            match &e.kind {
                Some(Kind::Created(c)) => {
                    headline.insert(c.story_id.clone(), c.headline.clone());
                    size.insert(c.story_id.clone(), 1);
                }
                Some(Kind::Updated(u)) => {
                    headline.insert(u.story_id.clone(), u.headline.clone());
                    size.insert(u.story_id.clone(), u.article_count);
                }
                _ => {}
            }
        }
        // Print after the whole input so children's headlines are known.
        for e in &events {
            let h = |id: &str| headline.get(id).cloned().unwrap_or_default();
            match &e.kind {
                Some(Kind::Split(sp)) => {
                    splits += 1;
                    if shown_splits < show {
                        shown_splits += 1;
                        println!("\nSPLIT @{offset}  {}", h(&sp.parent_story_id));
                        for c in &sp.children {
                            println!("   → ({:>3}) {}", c.article_ids.len(), h(&c.story_id));
                        }
                    }
                }
                Some(Kind::Merged(m)) => {
                    merges += 1;
                    if shown_merges < show {
                        shown_merges += 1;
                        for src in &m.source_story_ids {
                            println!(
                                "\nMERGE @{offset}  ({:>3}) {}",
                                size.get(src).copied().unwrap_or(0),
                                h(src)
                            );
                        }
                        println!(
                            "   into ({:>3}) {}",
                            size.get(&m.target_story_id).copied().unwrap_or(0),
                            h(&m.target_story_id)
                        );
                    }
                }
                _ => {}
            }
        }
    }
    println!(
        "\n{splits} splits, {merges} merges, {} open stories at end",
        engine.stories().len()
    );

    // Probe the end state: how separable are big stories, how close are pairs?
    let title = |k: u32| engine.articles()[&k].title.clone();
    let mut split_sims: Vec<(f32, u32)> = engine
        .stories()
        .iter()
        .filter(|(_, s)| s.originals.len() >= cfg.split_min_size)
        .filter_map(|(&k, s)| engine.split_plan(s).map(|p| (p.similarity, k)))
        .collect();
    split_sims.sort_by(|a, b| a.0.total_cmp(&b.0));
    if !split_sims.is_empty() {
        let q = |p: f64| split_sims[((split_sims.len() - 1) as f64 * p) as usize].0;
        println!(
            "\nprobe: 2-means halves similarity over {} large stories: min {:.2} p10 {:.2} p50 {:.2} p90 {:.2}",
            split_sims.len(),
            q(0.0),
            q(0.1),
            q(0.5),
            q(0.9)
        );
        for &(sim, k) in split_sims.iter().take(5) {
            let s = &engine.stories()[&k];
            let plan = engine.split_plan(s).expect("computed above");
            println!("  {sim:.2}  {}", title(s.headline));
            for g in &plan.groups {
                println!("        ({:>3}) {}", g.len(), title(g[0]));
            }
        }
    }
    let eligible: Vec<(u32, Vec<f32>)> = engine
        .stories()
        .iter()
        .filter(|(_, s)| s.originals.len() >= cfg.merge_min_size)
        .map(|(&k, s)| (k, s.centroid()))
        .collect();
    let mut pairs: Vec<(f32, u32, u32)> = Vec::new();
    for (i, (a, ca)) in eligible.iter().enumerate() {
        for (b, cb) in &eligible[i + 1..] {
            pairs.push((story_processor::ann::dot(ca, cb), *a, *b));
        }
    }
    pairs.sort_by(|x, y| y.0.total_cmp(&x.0));
    println!(
        "\nprobe: most similar story pairs among {} established stories:",
        eligible.len()
    );
    for &(sim, a, b) in pairs.iter().take(8) {
        let (sa, sb) = (&engine.stories()[&a], &engine.stories()[&b]);
        println!(
            "  {sim:.2}  ({:>3}) {}\n        ({:>3}) {}",
            sa.members.len(),
            title(sa.headline),
            sb.members.len(),
            title(sb.headline)
        );
    }
    Ok(())
}

fn sweep(
    inputs: &[EmbeddedArticle],
    centering: Option<Arc<Centering>>,
    neighbor: &[f32],
    centroid: &[f32],
) -> Result<()> {
    println!(
        "{:>8} {:>8} {:>8} {:>10} {:>12} {:>13} {:>8} {:>9}",
        "neighbor",
        "centroid",
        "stories",
        "singletons",
        "multi-source",
        "cross-lingual",
        "largest",
        "coverage"
    );
    for &n in neighbor {
        for &c in centroid {
            let cfg = Config {
                neighbor_similarity: n,
                centroid_similarity: c,
                centering: centering.clone(),
                ..Config::default()
            };
            let (engine, _) = run(inputs, cfg);
            let s = summarize(&engine);
            println!(
                "{n:>8.2} {c:>8.2} {:>8} {:>9.0}% {:>12} {:>13} {:>8} {:>8.1}%",
                s.stories,
                100.0 * s.singletons as f64 / s.stories.max(1) as f64,
                s.multi_source,
                s.cross_lingual,
                s.largest,
                100.0 * s.covered
            );
        }
    }
    Ok(())
}

fn recall(path: &Path, k: usize, holdout: f64, efs: &[usize]) -> Result<()> {
    let inputs = load(path)?;
    let vectors: Vec<&[f32]> = inputs.iter().map(|i| i.vector.as_slice()).collect();
    let dim = vectors.first().context("empty fixture")?.len();
    let split = ((1.0 - holdout) * vectors.len() as f64) as usize;
    let (indexed, queries) = vectors.split_at(split);

    let mut oracle = BruteForce::new(dim);
    let started = Instant::now();
    let mut hnsw = Hnsw::new(dim, HnswParams::default());
    for (i, v) in indexed.iter().enumerate() {
        hnsw.insert(i as u32, v);
    }
    let build = started.elapsed().as_secs_f64();
    for (i, v) in indexed.iter().enumerate() {
        oracle.insert(i as u32, v);
    }

    let started = Instant::now();
    let truth: Vec<Vec<u32>> = queries
        .iter()
        .map(|q| oracle.search(q, k).into_iter().map(|(id, _)| id).collect())
        .collect();
    let brute_qps = queries.len() as f64 / started.elapsed().as_secs_f64();

    println!(
        "{} indexed, {} queries, dim {dim}; HNSW build {:.2}s ({:.0} inserts/s); brute force {:.0} q/s",
        indexed.len(),
        queries.len(),
        build,
        indexed.len() as f64 / build,
        brute_qps
    );
    println!("{:>6} {:>10} {:>10}", "ef", "recall@k", "q/s");
    for &ef in efs {
        let mut params = hnsw.params().clone();
        params.ef_search = ef;
        let index = {
            // Same graph, different search beam.
            let mut h = Hnsw::new(dim, params);
            for (i, v) in indexed.iter().enumerate() {
                h.insert(i as u32, v);
            }
            h
        };
        let started = Instant::now();
        let mut hits = 0;
        for (q, t) in queries.iter().zip(&truth) {
            hits += index
                .search(q, k)
                .iter()
                .filter(|(id, _)| t.contains(id))
                .count();
        }
        let qps = queries.len() as f64 / started.elapsed().as_secs_f64();
        println!(
            "{ef:>6} {:>10.4} {qps:>10.0}",
            hits as f64 / (queries.len() * k) as f64
        );
    }
    Ok(())
}
