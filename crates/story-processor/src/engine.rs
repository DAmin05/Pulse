//! Online story clustering: embedded articles in, story events out.
//!
//! For each article, in log order:
//! 0. **Event time** advances the watermark (max event time − allowed
//!    lateness). Each time the watermark crosses a tick boundary, housekeeping
//!    closes stories idle past the timeout and evicts their articles, so state
//!    stays bounded. All of this is driven by event time in the log, never the
//!    wall clock.
//! 1. **Exact duplicate** (same id): ignored.
//! 2. **Near duplicate** (MinHash Jaccard ≥ `dup_jaccard` with a retained
//!    article): attached to that article's story as `is_duplicate`. Syndicated
//!    copies add coverage but stay out of the vector index and the centroid.
//! 3. Otherwise **vote**: the k nearest indexed articles with similarity ≥
//!    `neighbor_similarity` vote for their stories. The winner is joined if the
//!    article also fits its centroid (and, for established stories, has enough
//!    supporting neighbors and fits about as well as existing members). If no
//!    story qualifies, the article starts a new one, unless it is **late**
//!    (event time behind the watermark): late articles may join open stories
//!    but never create one; they are dropped for the caller to route to
//!    `articles.late`.
//!
//! Everything is a pure function of the input sequence: ordered maps, seeded
//! index, ties broken by the lowest key, ids derived from inputs. All mutable
//! state lives in [`State`], which snapshots byte for byte, so a restored
//! engine continues exactly as the original would have.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pulse_core::proto::v1::{
    ArticleAdded, CloseReason, EmbeddedArticle, StoryClosed, StoryCreated, StoryEvent,
    StoryUpdated, story_event::Kind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use xxhash_rust::xxh3::xxh3_64;

use crate::ann::{Hnsw, HnswParams, VectorIndex, dot};
use crate::centering::Centering;
use crate::minhash::{LshIndex, MinHasher, Signature};

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;

#[derive(Clone, Debug)]
pub struct Config {
    pub dup_jaccard: f32,
    pub k: usize,
    pub neighbor_similarity: f32,
    pub centroid_similarity: f32,
    /// A story with at least this many indexed members is "established"...
    pub established_size: usize,
    /// ...and joining it needs this many of its members among the neighbors.
    pub min_votes_established: usize,
    /// Joining needs centroid similarity ≥ (story's mean member similarity −
    /// margin). Stops generic articles diluting a tight story. `None` disables.
    pub cohesion_margin: Option<f32>,
    /// Watermark = max event time − this. `None`: nothing is ever late.
    pub allowed_lateness_ms: Option<i64>,
    /// Housekeeping runs each time the watermark crosses a multiple of this.
    pub tick_ms: i64,
    /// Stories with no article newer than watermark − this are closed.
    pub idle_close_ms: i64,
    /// Near-duplicate detection only looks this far back (event time).
    pub dedup_window_ms: i64,
    /// Exact-duplicate ids are remembered this long (event time).
    pub seen_ids_window_ms: i64,
    /// Rebuild the vector index once evicted entries exceed this share of it.
    pub rebuild_tombstone_ratio: f32,
    pub hnsw: HnswParams,
    /// Frozen per-language means; inputs from other models are rejected.
    pub centering: Option<Arc<Centering>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dup_jaccard: 0.8,
            k: 10,
            // Tuned on centered vectors (see `story-processor sweep --audit`):
            // 0.50 keeps the weakest joins on-story; 0.45 starts admitting
            // same-topic-different-event articles.
            neighbor_similarity: 0.50,
            centroid_similarity: 0.50,
            established_size: 3,
            min_votes_established: 2,
            cohesion_margin: Some(0.10),
            // Measured on the cold-start RSS fixture (72h backlog arriving at
            // once): 6h drops 52% of inputs as late, 24h drops 17%, 48h 5%.
            // 24h keeps "a day-old article can't announce a new story".
            allowed_lateness_ms: Some(24 * HOUR_MS),
            tick_ms: 10 * MINUTE_MS,
            idle_close_ms: 48 * HOUR_MS,
            dedup_window_ms: 72 * HOUR_MS,
            seen_ids_window_ms: 7 * 24 * HOUR_MS,
            rebuild_tombstone_ratio: 0.2,
            hnsw: HnswParams::default(),
            centering: None,
        }
    }
}

impl Config {
    /// Hash of everything that affects outputs. Snapshots record it, so state
    /// built under one configuration is never resumed under another.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        // Two tuples: Debug is only implemented for tuples up to 12 elements.
        h.update(format!(
            "{:?}{:?}",
            (
                self.dup_jaccard,
                self.k,
                self.neighbor_similarity,
                self.centroid_similarity,
                self.established_size,
                self.min_votes_established,
                self.cohesion_margin,
            ),
            (
                self.allowed_lateness_ms,
                self.tick_ms,
                self.idle_close_ms,
                self.dedup_window_ms,
                self.seen_ids_window_ms,
                self.rebuild_tombstone_ratio,
                &self.hnsw,
            )
        ));
        if let Some(c) = &self.centering {
            h.update(c.model_version.as_bytes());
            for v in std::iter::once(&c.global).chain(c.per_lang.values()) {
                for x in v {
                    h.update(x.to_le_bytes());
                }
            }
            h.update(format!("{:?}", c.per_lang.keys().collect::<Vec<_>>()));
        }
        hex::encode(&h.finalize()[..12])
    }
}

/// What happened to one input, for metrics, routing and evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Invalid,
    ExactDuplicate,
    NearDuplicate,
    Joined,
    Created,
    /// Late and matched no open story: no story created; route to articles.late.
    LateDropped,
}

#[derive(Debug)]
pub struct Processed {
    pub outcome: Outcome,
    /// Event time was behind the watermark when the input arrived.
    pub late: bool,
    /// Events caused by this input, including any housekeeping it triggered.
    pub events: Vec<StoryEvent>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ArticleState {
    pub id: String,
    pub story: u32,
    pub event_time_ms: i64,
    pub source_id: String,
    pub lang: String,
    pub title: String,
    pub duplicate_of: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Story {
    pub id: String,
    pub members: Vec<u32>,
    /// Non-duplicate members (the ones in the vector index).
    pub originals: Vec<u32>,
    centroid_sum: Vec<f32>,
    pub sources: BTreeSet<String>,
    pub langs: BTreeSet<String>,
    pub headline: u32,
    pub first_event_ms: i64,
    pub last_event_ms: i64,
    /// Sum and count of members' centroid similarity at the time they joined.
    cohesion_sum: f32,
    cohesion_n: u32,
}

impl Story {
    pub fn centroid(&self) -> Vec<f32> {
        let norm = dot(&self.centroid_sum, &self.centroid_sum)
            .sqrt()
            .max(1e-12);
        self.centroid_sum.iter().map(|x| x / norm).collect()
    }

    /// Mean similarity of joined members to the centroid; `None` until two joins.
    pub fn cohesion(&self) -> Option<f32> {
        (self.cohesion_n >= 2).then(|| self.cohesion_sum / self.cohesion_n as f32)
    }
}

/// All mutable engine state. Snapshotting this is a complete checkpoint.
#[derive(Serialize, Deserialize)]
pub struct State {
    dim: Option<usize>,
    next_key: u32,
    articles: BTreeMap<u32, ArticleState>,
    /// Keyed by the seed article's key, so iteration order is creation order.
    stories: BTreeMap<u32, Story>,
    /// xxh3(article id) → event time, for exact-duplicate detection.
    seen_ids: BTreeMap<u64, i64>,
    lsh: LshIndex,
    ann: Option<Hnsw>,
    /// Evicted articles still present in `ann`.
    tombstones: usize,
    max_event_ms: i64,
    watermark_ms: i64,
    last_tick: i64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            dim: None,
            next_key: 0,
            articles: BTreeMap::new(),
            stories: BTreeMap::new(),
            seen_ids: BTreeMap::new(),
            lsh: LshIndex::default(),
            ann: None,
            tombstones: 0,
            max_event_ms: i64::MIN,
            watermark_ms: i64::MIN,
            last_tick: i64::MIN,
        }
    }
}

pub struct Engine {
    cfg: Config,
    minhasher: MinHasher,
    s: State,
}

impl Engine {
    pub fn new(cfg: Config) -> Self {
        Self::from_state(cfg, State::default())
    }

    /// Resumes from deserialized state (derived lookups are rebuilt).
    pub fn from_state(cfg: Config, mut state: State) -> Self {
        if let Some(ann) = state.ann.as_mut() {
            ann.reindex();
        }
        Self {
            cfg,
            minhasher: MinHasher::default(),
            s: state,
        }
    }

    pub fn state(&self) -> &State {
        &self.s
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn articles(&self) -> &BTreeMap<u32, ArticleState> {
        &self.s.articles
    }

    pub fn stories(&self) -> &BTreeMap<u32, Story> {
        &self.s.stories
    }

    /// `i64::MIN` until the first input.
    pub fn watermark_ms(&self) -> i64 {
        self.s.watermark_ms
    }

    pub fn index_len(&self) -> usize {
        self.s.ann.as_ref().map_or(0, |a| a.len())
    }

    pub fn tombstones(&self) -> usize {
        self.s.tombstones
    }

    /// The (centered) vector stored for an indexed article.
    pub fn indexed_vector(&self, key: u32) -> Option<&[f32]> {
        self.s.ann.as_ref()?.vector(key)
    }

    pub fn process(&mut self, input: &EmbeddedArticle, input_offset: i64) -> Processed {
        let invalid = || Processed {
            outcome: Outcome::Invalid,
            late: false,
            events: Vec::new(),
        };
        let Some(article) = &input.article else {
            return invalid();
        };
        if input.vector.is_empty() || article.id.is_empty() {
            return invalid();
        }
        let centered;
        let vector: &[f32] = match &self.cfg.centering {
            Some(c) if c.model_version != input.model_version => return invalid(),
            Some(c) => {
                centered = c.apply(&article.lang, &input.vector);
                &centered
            }
            None => &input.vector,
        };
        let dim = *self.s.dim.get_or_insert(vector.len());
        if vector.len() != dim {
            return invalid();
        }

        let event_time = article.published_at_ms;
        let mut out = Emitter::new(input_offset);
        self.advance(event_time, &mut out);
        out.watermark_ms = self.s.watermark_ms;
        out.event_time_ms = event_time;
        let late = event_time < self.s.watermark_ms;
        let done = |outcome, out: Emitter| Processed {
            outcome,
            late,
            events: out.events,
        };

        // 1. Exact duplicate.
        let id_hash = xxh3_64(article.id.as_bytes());
        if self.s.seen_ids.contains_key(&id_hash) {
            return done(Outcome::ExactDuplicate, out);
        }
        self.s.seen_ids.insert(id_hash, event_time);

        let key = self.s.next_key;
        let text = if article.summary.is_empty() {
            article.title.clone()
        } else {
            format!("{}\n{}", article.title, article.summary)
        };
        let signature = self.minhasher.signature(&text);
        let mut state = ArticleState {
            id: article.id.clone(),
            story: key,
            event_time_ms: event_time,
            source_id: article.source_id.clone(),
            lang: article.lang.clone(),
            title: article.title.clone(),
            duplicate_of: None,
        };

        // 2. Near duplicate.
        let duplicate = signature
            .as_ref()
            .and_then(|sig| self.s.lsh.find_duplicate(sig, self.cfg.dup_jaccard));
        if let Some((original, jaccard)) = duplicate {
            let original_state = &self.s.articles[&original];
            let story_key = original_state.story;
            let original_id = original_state.id.clone();
            state.story = story_key;
            state.duplicate_of = Some(original);
            self.push_article(key, state, signature);
            let grew = self.add_member(story_key, key, None);
            out.emit(Kind::ArticleAdded(ArticleAdded {
                story_id: self.s.stories[&story_key].id.clone(),
                article_id: article.id.clone(),
                score: jaccard,
                is_duplicate: true,
                duplicate_of: original_id,
                late,
            }));
            if grew {
                out.emit(self.story_updated(story_key));
            }
            return done(Outcome::NearDuplicate, out);
        }

        // 3. Vote among nearest neighbors.
        let ann = self
            .s
            .ann
            .get_or_insert_with(|| Hnsw::new(dim, self.cfg.hnsw.clone()));
        // Over-fetch when evicted entries may occupy result slots.
        let fetch = if self.s.tombstones > 0 {
            self.cfg.k * 2
        } else {
            self.cfg.k
        };
        // story → (summed similarity, number of member neighbors)
        let mut votes: BTreeMap<u32, (f32, usize)> = BTreeMap::new();
        for (neighbor, similarity) in ann
            .search(vector, fetch)
            .into_iter()
            .filter(|(n, _)| self.s.articles.contains_key(n))
            .take(self.cfg.k)
        {
            if similarity >= self.cfg.neighbor_similarity {
                let v = votes.entry(self.s.articles[&neighbor].story).or_default();
                v.0 += similarity;
                v.1 += 1;
            }
        }
        // Highest vote first; ties to the oldest story (lowest key).
        let mut ranked: Vec<(u32, (f32, usize))> = votes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
        let joined = ranked.into_iter().find_map(|(story_key, (score, count))| {
            let story = &self.s.stories[&story_key];
            if story.originals.len() >= self.cfg.established_size
                && count < self.cfg.min_votes_established
            {
                return None;
            }
            let fit = dot(vector, &story.centroid());
            let cohesive = match (self.cfg.cohesion_margin, story.cohesion()) {
                (Some(margin), Some(cohesion)) => fit >= cohesion - margin,
                _ => true,
            };
            (fit >= self.cfg.centroid_similarity && cohesive).then_some((story_key, score, fit))
        });

        match joined {
            Some((story_key, score, fit)) => {
                state.story = story_key;
                self.push_article(key, state, signature);
                self.ann_insert(key, vector);
                let grew = self.add_member(story_key, key, Some((vector, Some(fit))));
                out.emit(Kind::ArticleAdded(ArticleAdded {
                    story_id: self.s.stories[&story_key].id.clone(),
                    article_id: article.id.clone(),
                    score,
                    is_duplicate: false,
                    duplicate_of: String::new(),
                    late,
                }));
                let refreshed = self.refresh_headline(story_key);
                if grew || refreshed {
                    out.emit(self.story_updated(story_key));
                }
                done(Outcome::Joined, out)
            }
            // Late news doesn't open new stories.
            None if late => done(Outcome::LateDropped, out),
            None => {
                let story_id = format!("st_{}", &article.id[..article.id.len().min(16)]);
                self.push_article(key, state, signature);
                self.ann_insert(key, vector);
                self.s.stories.insert(
                    key,
                    Story {
                        id: story_id.clone(),
                        members: Vec::new(),
                        originals: Vec::new(),
                        centroid_sum: vec![0.0; dim],
                        sources: BTreeSet::new(),
                        langs: BTreeSet::new(),
                        headline: key,
                        first_event_ms: event_time,
                        last_event_ms: event_time,
                        cohesion_sum: 0.0,
                        cohesion_n: 0,
                    },
                );
                self.add_member(key, key, Some((vector, None)));
                out.emit(Kind::Created(StoryCreated {
                    story_id,
                    seed_article_id: article.id.clone(),
                    headline: article.title.clone(),
                    lang: article.lang.clone(),
                }));
                done(Outcome::Created, out)
            }
        }
    }

    /// Advances event time and runs housekeeping when the watermark crosses a tick.
    fn advance(&mut self, event_time: i64, out: &mut Emitter) {
        self.s.max_event_ms = self.s.max_event_ms.max(event_time);
        if let Some(lateness) = self.cfg.allowed_lateness_ms {
            let candidate = self.s.max_event_ms.saturating_sub(lateness);
            self.s.watermark_ms = self.s.watermark_ms.max(candidate);
        }
        if self.s.watermark_ms == i64::MIN {
            return;
        }
        let tick = self.s.watermark_ms.div_euclid(self.cfg.tick_ms);
        if tick > self.s.last_tick {
            self.s.last_tick = tick;
            self.housekeeping(out);
        }
    }

    /// Closes idle stories, evicts their articles, ages out dedup state and
    /// rebuilds the vector index when it holds too many evicted entries.
    fn housekeeping(&mut self, out: &mut Emitter) {
        let watermark = self.s.watermark_ms;
        out.event_time_ms = watermark;
        out.watermark_ms = watermark;

        let idle_before = watermark.saturating_sub(self.cfg.idle_close_ms);
        let idle: Vec<u32> = self
            .s
            .stories
            .iter()
            .filter(|(_, s)| s.last_event_ms < idle_before)
            .map(|(&k, _)| k)
            .collect();
        for story_key in idle {
            let story = self.s.stories.remove(&story_key).expect("listed above");
            out.emit(Kind::Closed(StoryClosed {
                story_id: story.id,
                reason: CloseReason::Idle as i32,
            }));
            for member in &story.members {
                self.s.articles.remove(member);
            }
            self.s.tombstones += story.originals.len();
        }

        let dedup_before = watermark.saturating_sub(self.cfg.dedup_window_ms);
        let articles = &self.s.articles;
        self.s.lsh.retain(|k| {
            articles
                .get(&k)
                .is_some_and(|a| a.event_time_ms >= dedup_before)
        });
        let seen_before = watermark.saturating_sub(self.cfg.seen_ids_window_ms);
        self.s.seen_ids.retain(|_, t| *t >= seen_before);

        if let Some(ann) = &self.s.ann {
            if self.s.tombstones as f32 > self.cfg.rebuild_tombstone_ratio * ann.len() as f32 {
                let live = &self.s.articles;
                self.s.ann = Some(ann.rebuilt(|k| live.contains_key(&k)));
                self.s.tombstones = 0;
            }
        }
    }

    fn push_article(&mut self, key: u32, state: ArticleState, signature: Option<Signature>) {
        self.s.next_key += 1;
        self.s.articles.insert(key, state);
        if let Some(sig) = signature {
            self.s.lsh.insert(key, sig);
        }
    }

    fn ann_insert(&mut self, key: u32, vector: &[f32]) {
        self.s
            .ann
            .as_mut()
            .expect("created before first insert")
            .insert(key, vector);
    }

    /// Adds a member; returns true when its source or language set grew.
    /// `indexed` is the member's vector and, for joins, its centroid fit.
    fn add_member(
        &mut self,
        story_key: u32,
        key: u32,
        indexed: Option<(&[f32], Option<f32>)>,
    ) -> bool {
        let article = &self.s.articles[&key];
        let story = self.s.stories.get_mut(&story_key).expect("story exists");
        story.members.push(key);
        story.first_event_ms = story.first_event_ms.min(article.event_time_ms);
        story.last_event_ms = story.last_event_ms.max(article.event_time_ms);
        if let Some((v, fit)) = indexed {
            story.originals.push(key);
            story
                .centroid_sum
                .iter_mut()
                .zip(v)
                .for_each(|(s, x)| *s += x);
            if let Some(fit) = fit {
                story.cohesion_sum += fit;
                story.cohesion_n += 1;
            }
        }
        let new_source = story.sources.insert(article.source_id.clone());
        let new_lang = story.langs.insert(article.lang.clone());
        // Only the seed is excluded: a story's first member isn't "growth".
        story.members.len() > 1 && (new_source || new_lang)
    }

    /// Re-picks the headline as the member nearest the centroid, at power-of-two
    /// sizes (cheap and enough to track a story as it settles).
    fn refresh_headline(&mut self, story_key: u32) -> bool {
        let story = &self.s.stories[&story_key];
        if !story.originals.len().is_power_of_two() {
            return false;
        }
        let centroid = story.centroid();
        let ann = self.s.ann.as_ref().expect("index exists");
        let best = story
            .originals
            .iter()
            .map(|&k| (k, dot(&centroid, ann.vector(k).expect("indexed"))))
            .fold(None, |best: Option<(u32, f32)>, (k, s)| match best {
                Some((_, bs)) if bs >= s => best,
                _ => Some((k, s)),
            })
            .map(|(k, _)| k)
            .unwrap_or(story.headline);
        let story = self.s.stories.get_mut(&story_key).expect("exists");
        let changed = story.headline != best;
        story.headline = best;
        changed
    }

    fn story_updated(&self, story_key: u32) -> Kind {
        let story = &self.s.stories[&story_key];
        let headline = &self.s.articles[&story.headline];
        Kind::Updated(StoryUpdated {
            story_id: story.id.clone(),
            headline: headline.title.clone(),
            headline_article_id: headline.id.clone(),
            article_count: story.members.len() as u32,
            source_count: story.sources.len() as u32,
            langs: story.langs.iter().cloned().collect(),
            centroid: story.centroid(),
        })
    }
}

/// Builds events with deterministic ids derived from the input position.
struct Emitter {
    input_offset: i64,
    event_time_ms: i64,
    watermark_ms: i64,
    events: Vec<StoryEvent>,
}

impl Emitter {
    fn new(input_offset: i64) -> Self {
        Self {
            input_offset,
            event_time_ms: 0,
            watermark_ms: 0,
            events: Vec::new(),
        }
    }

    fn emit(&mut self, kind: Kind) {
        let seq = self.events.len() as u32;
        let mut h = Sha256::new();
        h.update(self.input_offset.to_le_bytes());
        h.update(seq.to_le_bytes());
        self.events.push(StoryEvent {
            event_id: hex::encode(&h.finalize()[..16]),
            event_time_ms: self.event_time_ms,
            watermark_ms: self.watermark_ms,
            input_offset: self.input_offset,
            seq,
            kind: Some(kind),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulse_core::proto::v1::Article;

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = dot(v, v).sqrt();
        v.iter().map(|x| x / n).collect()
    }

    fn input_at(id: &str, title: &str, v: &[f32], event_time_ms: i64) -> EmbeddedArticle {
        EmbeddedArticle {
            article: Some(Article {
                id: id.into(),
                title: title.into(),
                source_id: format!("src-{id}"),
                lang: "en".into(),
                published_at_ms: event_time_ms,
                ..Default::default()
            }),
            vector: unit(v),
            model_version: "test".into(),
        }
    }

    fn input(id: &str, title: &str, source: &str, lang: &str, v: &[f32]) -> EmbeddedArticle {
        let mut i = input_at(id, title, v, 1_000);
        let a = i.article.as_mut().unwrap();
        a.source_id = source.into();
        a.lang = lang.into();
        i
    }

    fn kinds(events: &[StoryEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e.kind.as_ref().unwrap() {
                Kind::Created(_) => "created",
                Kind::ArticleAdded(a) if a.is_duplicate => "dup",
                Kind::ArticleAdded(_) => "added",
                Kind::Updated(_) => "updated",
                Kind::Closed(_) => "closed",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn clusters_duplicates_and_new_stories() {
        let mut e = Engine::new(Config::default());
        let quake = [1.0, 0.1, 0.0, 0.0];
        let quake_es = [1.0, 0.15, 0.05, 0.0];
        let rates = [0.0, 0.0, 1.0, 0.2];

        let p = e.process(
            &input("a1", "Strong quake hits Japan coast", "bbc", "en", &quake),
            0,
        );
        assert_eq!(
            (p.outcome, kinds(&p.events)),
            (Outcome::Created, vec!["created"])
        );

        let p = e.process(
            &input("a2", "Fuerte sismo sacude Japón", "elpais", "es", &quake_es),
            1,
        );
        assert_eq!(p.outcome, Outcome::Joined);
        assert_eq!(kinds(&p.events), vec!["added", "updated"]); // new source + language

        let p = e.process(
            &input("a3", "Strong quake hits Japan coast!", "cnn", "en", &quake),
            2,
        );
        assert_eq!(p.outcome, Outcome::NearDuplicate);
        assert_eq!(kinds(&p.events), vec!["dup", "updated"]);

        let p = e.process(
            &input("a4", "Central bank raises rates", "ft", "en", &rates),
            3,
        );
        assert_eq!(p.outcome, Outcome::Created);

        let p = e.process(&input("a1", "again", "bbc", "en", &quake), 4);
        assert_eq!((p.outcome, p.events.len()), (Outcome::ExactDuplicate, 0));

        assert_eq!(e.stories().len(), 2);
        let quake_story = &e.stories()[&0];
        assert_eq!(quake_story.members, vec![0, 1, 2]);
        assert_eq!(quake_story.originals, vec![0, 1]);
        assert_eq!(quake_story.langs.len(), 2);
    }

    #[test]
    fn established_stories_need_several_supporting_neighbors() {
        // A, B, C form one story; X is close to A only (and to the centroid).
        let story = [
            ("a", "Alpha one", [1.0, 0.3, 0.0]),
            ("b", "Bravo two", [1.0, -0.3, 0.0]),
            ("c", "Charlie three", [1.0, -0.1, 0.4]),
        ];
        let x = input("x", "Xray four", "s", "en", &[0.6, 0.8, 0.0]);
        let run = |min_votes| {
            let mut e = Engine::new(Config {
                min_votes_established: min_votes,
                cohesion_margin: None,
                ..Config::default()
            });
            for (i, (id, title, v)) in story.iter().enumerate() {
                e.process(&input(id, title, "s", "en", v), i as i64);
            }
            assert_eq!(e.stories().len(), 1, "fixture should form one story");
            e.process(&x, 3).outcome
        };
        assert_eq!(run(1), Outcome::Joined);
        assert_eq!(run(2), Outcome::Created);
    }

    #[test]
    fn late_articles_join_open_stories_but_never_create_them() {
        let mut e = Engine::new(Config::default());
        let quake = [1.0, 0.1, 0.0];
        let other = [0.0, 1.0, 0.2];
        let now = 100 * HOUR_MS;

        e.process(&input_at("q1", "Quake hits Japan", &quake, now), 0);
        assert_eq!(e.watermark_ms(), now - 24 * HOUR_MS);

        // 30h old but on an open story: joins, flagged late.
        let p = e.process(
            &input_at("q2", "Japan quake aftermath", &quake, now - 30 * HOUR_MS),
            1,
        );
        assert_eq!((p.outcome, p.late), (Outcome::Joined, true));
        let Some(Kind::ArticleAdded(added)) = &p.events[0].kind else {
            panic!("expected ArticleAdded");
        };
        assert!(added.late);

        // 30h old and unrelated: would create a story, so it is dropped instead.
        let p = e.process(
            &input_at("o1", "Unrelated old news", &other, now - 30 * HOUR_MS),
            2,
        );
        assert_eq!((p.outcome, p.late), (Outcome::LateDropped, true));
        assert_eq!(e.stories().len(), 1);

        // Within the allowed lateness: normal.
        let p = e.process(
            &input_at("o2", "Recent other news", &other, now - HOUR_MS),
            3,
        );
        assert_eq!((p.outcome, p.late), (Outcome::Created, false));

        // Disabling lateness makes nothing late.
        let mut e = Engine::new(Config {
            allowed_lateness_ms: None,
            ..Config::default()
        });
        e.process(&input_at("q1", "Quake hits Japan", &quake, now), 0);
        let p = e.process(&input_at("o1", "Unrelated old news", &other, 0), 1);
        assert_eq!((p.outcome, p.late), (Outcome::Created, false));
    }

    #[test]
    fn idle_stories_close_and_leave_memory() {
        let mut e = Engine::new(Config {
            rebuild_tombstone_ratio: 0.0, // rebuild on any eviction
            ..Config::default()
        });
        let t0 = 100 * HOUR_MS;
        e.process(&input_at("a", "Old story", &[1.0, 0.0, 0.0], t0), 0);
        e.process(&input_at("b", "Old story again", &[1.0, 0.05, 0.0], t0), 1);
        assert_eq!(e.stories().len(), 1);

        // 80h later: watermark = t0 + 56h > last event + 48h idle timeout.
        let p = e.process(
            &input_at("c", "Fresh news", &[0.0, 1.0, 0.0], t0 + 80 * HOUR_MS),
            2,
        );
        assert_eq!(kinds(&p.events), vec!["closed", "created"]);
        let closed = &p.events[0];
        assert_eq!(closed.event_time_ms, closed.watermark_ms); // stamped at the watermark
        assert_eq!(e.stories().len(), 1);
        assert_eq!(e.articles().len(), 1, "closed story's articles evicted");
        assert_eq!((e.index_len(), e.tombstones()), (1, 0), "index rebuilt");

        // A vector near the evicted story no longer finds it.
        let p = e.process(
            &input_at(
                "d",
                "Old story revisited",
                &[1.0, 0.0, 0.0],
                t0 + 80 * HOUR_MS,
            ),
            3,
        );
        assert_eq!(p.outcome, Outcome::Created);
    }

    #[test]
    fn event_ids_are_deterministic_and_unique() {
        let run = || {
            let mut e = Engine::new(Config::default());
            let mut all = Vec::new();
            for i in 0..20u32 {
                let v = [1.0, (i % 3) as f32, (i % 5) as f32 * 0.1, 0.3];
                let p = e.process(
                    &input(&format!("id{i}"), &format!("title {i}"), "s", "en", &v),
                    i as i64,
                );
                all.extend(p.events);
            }
            all
        };
        let (a, b) = (run(), run());
        assert_eq!(a, b);
        let ids: BTreeSet<_> = a.iter().map(|e| &e.event_id).collect();
        assert_eq!(ids.len(), a.len());
    }

    #[test]
    fn rejects_invalid_inputs() {
        let mut e = Engine::new(Config::default());
        assert_eq!(
            e.process(&EmbeddedArticle::default(), 0).outcome,
            Outcome::Invalid
        );
        e.process(&input("a", "t", "s", "en", &[1.0, 0.0]), 1);
        let p = e.process(&input("b", "t2", "s", "en", &[1.0, 0.0, 0.0]), 2);
        assert_eq!(p.outcome, Outcome::Invalid); // dimension mismatch
    }

    #[test]
    fn fingerprint_tracks_output_relevant_config() {
        let a = Config::default();
        let b = Config {
            neighbor_similarity: 0.51,
            ..Config::default()
        };
        assert_eq!(a.fingerprint(), Config::default().fingerprint());
        assert_ne!(a.fingerprint(), b.fingerprint());
    }
}
