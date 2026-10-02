//! Online story clustering: embedded articles in, story events out.
//!
//! For each article, in log order:
//! 1. **Exact duplicate** (same id): ignored.
//! 2. **Near duplicate** (MinHash Jaccard ≥ `dup_jaccard` with an earlier
//!    article): attached to that article's story as `is_duplicate`. Syndicated
//!    wire copies add coverage, but they stay out of the vector index and the
//!    centroid so they can't drag a story around.
//! 3. Otherwise **vote**: the k nearest indexed articles with similarity ≥
//!    `neighbor_similarity` vote for their stories, weighted by similarity. The
//!    winner is joined if the article is also close to its centroid
//!    (`centroid_similarity`), which stops chains of pairwise-similar articles
//!    from drifting into one giant story. If no story qualifies, the article
//!    starts a new one.
//!
//! Everything is a pure function of the input sequence: ordered maps, seeded
//! index, ties broken by the lowest key, ids derived from inputs. Replaying the
//! same log reproduces the same events byte for byte.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use pulse_core::proto::v1::{
    ArticleAdded, EmbeddedArticle, StoryCreated, StoryEvent, StoryUpdated, story_event::Kind,
};
use sha2::{Digest, Sha256};

use crate::ann::{Hnsw, HnswParams, VectorIndex, dot};
use crate::centering::Centering;
use crate::minhash::{LshIndex, MinHasher};

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
            hnsw: HnswParams::default(),
            centering: None,
        }
    }
}

/// What happened to one input, for metrics and evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Invalid,
    ExactDuplicate,
    NearDuplicate,
    Joined,
    Created,
}

#[derive(Debug)]
pub struct ArticleState {
    pub id: String,
    pub story: u32,
    pub event_time_ms: i64,
    pub source_id: String,
    pub lang: String,
    pub title: String,
    pub duplicate_of: Option<u32>,
}

#[derive(Debug)]
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
    /// Mean similarity of joined members to the centroid; `None` until two joins.
    pub fn cohesion(&self) -> Option<f32> {
        (self.cohesion_n >= 2).then(|| self.cohesion_sum / self.cohesion_n as f32)
    }

    pub fn centroid(&self) -> Vec<f32> {
        let norm = dot(&self.centroid_sum, &self.centroid_sum)
            .sqrt()
            .max(1e-12);
        self.centroid_sum.iter().map(|x| x / norm).collect()
    }
}

pub struct Engine {
    cfg: Config,
    dim: Option<usize>,
    minhasher: MinHasher,
    lsh: LshIndex,
    ann: Option<Hnsw>,
    /// Dense by internal article key (`u32`, assignment order).
    pub articles: Vec<ArticleState>,
    by_id: HashMap<String, u32>,
    /// Keyed by the seed article's key, so iteration order is creation order.
    pub stories: BTreeMap<u32, Story>,
}

impl Engine {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            dim: None,
            minhasher: MinHasher::default(),
            lsh: LshIndex::default(),
            ann: None,
            articles: Vec::new(),
            by_id: HashMap::new(),
            stories: BTreeMap::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// The (centered) vector stored for an indexed article.
    pub fn indexed_vector(&self, key: u32) -> Option<&[f32]> {
        self.ann.as_ref()?.vector(key)
    }

    pub fn process(
        &mut self,
        input: &EmbeddedArticle,
        input_offset: i64,
    ) -> (Outcome, Vec<StoryEvent>) {
        let Some(article) = &input.article else {
            return (Outcome::Invalid, Vec::new());
        };
        if input.vector.is_empty() || article.id.is_empty() {
            return (Outcome::Invalid, Vec::new());
        }
        let centered;
        let vector: &[f32] = match &self.cfg.centering {
            Some(c) if c.model_version != input.model_version => {
                return (Outcome::Invalid, Vec::new());
            }
            Some(c) => {
                centered = c.apply(&article.lang, &input.vector);
                &centered
            }
            None => &input.vector,
        };
        let dim = *self.dim.get_or_insert(vector.len());
        if vector.len() != dim {
            return (Outcome::Invalid, Vec::new());
        }
        if self.by_id.contains_key(&article.id) {
            return (Outcome::ExactDuplicate, Vec::new());
        }

        let key = self.articles.len() as u32;
        let text = if article.summary.is_empty() {
            article.title.clone()
        } else {
            format!("{}\n{}", article.title, article.summary)
        };
        let signature = self.minhasher.signature(&text);
        let mut state = ArticleState {
            id: article.id.clone(),
            story: key,
            event_time_ms: article.published_at_ms,
            source_id: article.source_id.clone(),
            lang: article.lang.clone(),
            title: article.title.clone(),
            duplicate_of: None,
        };
        let mut out = Emitter::new(input_offset, article.published_at_ms);

        // 2. Near duplicate.
        let duplicate = signature
            .as_ref()
            .and_then(|sig| self.lsh.find_duplicate(sig, self.cfg.dup_jaccard));
        if let Some((original, jaccard)) = duplicate {
            let story_key = self.articles[original as usize].story;
            state.story = story_key;
            state.duplicate_of = Some(original);
            let original_id = self.articles[original as usize].id.clone();
            self.push_article(key, state, signature);
            let grew = self.add_member(story_key, key, None);
            let story_id = self.stories[&story_key].id.clone();
            out.emit(Kind::ArticleAdded(ArticleAdded {
                story_id,
                article_id: article.id.clone(),
                score: jaccard,
                is_duplicate: true,
                duplicate_of: original_id,
            }));
            if grew {
                out.emit(self.story_updated(story_key));
            }
            return (Outcome::NearDuplicate, out.events);
        }

        // 3. Vote among nearest neighbors.
        let ann = self
            .ann
            .get_or_insert_with(|| Hnsw::new(dim, self.cfg.hnsw.clone()));
        // story → (summed similarity, number of member neighbors)
        let mut votes: BTreeMap<u32, (f32, usize)> = BTreeMap::new();
        for (neighbor, similarity) in ann.search(vector, self.cfg.k) {
            if similarity >= self.cfg.neighbor_similarity {
                let v = votes
                    .entry(self.articles[neighbor as usize].story)
                    .or_default();
                v.0 += similarity;
                v.1 += 1;
            }
        }
        // Highest vote first; ties to the oldest story (lowest key).
        let mut ranked: Vec<(u32, (f32, usize))> = votes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
        let joined = ranked.into_iter().find_map(|(story_key, (score, count))| {
            let story = &self.stories[&story_key];
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

        let outcome = match joined {
            Some((story_key, score, fit)) => {
                state.story = story_key;
                self.push_article(key, state, signature);
                self.ann_insert(key, vector);
                let grew = self.add_member(story_key, key, Some((vector, Some(fit))));
                out.emit(Kind::ArticleAdded(ArticleAdded {
                    story_id: self.stories[&story_key].id.clone(),
                    article_id: article.id.clone(),
                    score,
                    is_duplicate: false,
                    duplicate_of: String::new(),
                }));
                let refreshed = self.refresh_headline(story_key);
                if grew || refreshed {
                    out.emit(self.story_updated(story_key));
                }
                Outcome::Joined
            }
            None => {
                let story_id = format!("st_{}", &article.id[..article.id.len().min(16)]);
                self.push_article(key, state, signature);
                self.ann_insert(key, vector);
                self.stories.insert(
                    key,
                    Story {
                        id: story_id.clone(),
                        members: Vec::new(),
                        originals: Vec::new(),
                        centroid_sum: vec![0.0; dim],
                        sources: BTreeSet::new(),
                        langs: BTreeSet::new(),
                        headline: key,
                        first_event_ms: article.published_at_ms,
                        last_event_ms: article.published_at_ms,
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
                Outcome::Created
            }
        };
        (outcome, out.events)
    }

    fn push_article(
        &mut self,
        key: u32,
        state: ArticleState,
        signature: Option<crate::minhash::Signature>,
    ) {
        self.by_id.insert(state.id.clone(), key);
        self.articles.push(state);
        if let Some(sig) = signature {
            self.lsh.insert(key, sig);
        }
    }

    fn ann_insert(&mut self, key: u32, vector: &[f32]) {
        self.ann
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
        let article = &self.articles[key as usize];
        let story = self.stories.get_mut(&story_key).expect("story exists");
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
        let story = &self.stories[&story_key];
        if !story.originals.len().is_power_of_two() {
            return false;
        }
        let centroid = story.centroid();
        let ann = self.ann.as_ref().expect("index exists");
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
        let story = self.stories.get_mut(&story_key).expect("exists");
        let changed = story.headline != best;
        story.headline = best;
        changed
    }

    fn story_updated(&self, story_key: u32) -> Kind {
        let story = &self.stories[&story_key];
        let headline = &self.articles[story.headline as usize];
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
    events: Vec<StoryEvent>,
}

impl Emitter {
    fn new(input_offset: i64, event_time_ms: i64) -> Self {
        Self {
            input_offset,
            event_time_ms,
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
            watermark_ms: 0, // phase 4
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

    fn input(id: &str, title: &str, source: &str, lang: &str, v: &[f32]) -> EmbeddedArticle {
        EmbeddedArticle {
            article: Some(Article {
                id: id.into(),
                title: title.into(),
                source_id: source.into(),
                lang: lang.into(),
                published_at_ms: 1_000,
                ..Default::default()
            }),
            vector: unit(v),
            model_version: "test".into(),
        }
    }

    fn kinds(events: &[StoryEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e.kind.as_ref().unwrap() {
                Kind::Created(_) => "created",
                Kind::ArticleAdded(a) if a.is_duplicate => "dup",
                Kind::ArticleAdded(_) => "added",
                Kind::Updated(_) => "updated",
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

        let (o, ev) = e.process(
            &input("a1", "Strong quake hits Japan coast", "bbc", "en", &quake),
            0,
        );
        assert_eq!((o, kinds(&ev)), (Outcome::Created, vec!["created"]));

        let (o, ev) = e.process(
            &input("a2", "Fuerte sismo sacude Japón", "elpais", "es", &quake_es),
            1,
        );
        assert_eq!(o, Outcome::Joined);
        assert_eq!(kinds(&ev), vec!["added", "updated"]); // new source + language

        let (o, ev) = e.process(
            &input("a3", "Strong quake hits Japan coast!", "cnn", "en", &quake),
            2,
        );
        assert_eq!(o, Outcome::NearDuplicate);
        assert_eq!(kinds(&ev), vec!["dup", "updated"]);

        let (o, _) = e.process(
            &input("a4", "Central bank raises rates", "ft", "en", &rates),
            3,
        );
        assert_eq!(o, Outcome::Created);

        let (o, ev) = e.process(&input("a1", "again", "bbc", "en", &quake), 4);
        assert_eq!((o, ev.len()), (Outcome::ExactDuplicate, 0));

        assert_eq!(e.stories.len(), 2);
        let quake_story = &e.stories[&0];
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
            assert_eq!(e.stories.len(), 1, "fixture should form one story");
            e.process(&x, 3).0
        };
        assert_eq!(run(1), Outcome::Joined);
        assert_eq!(run(2), Outcome::Created);
    }

    #[test]
    fn event_ids_are_deterministic_and_unique() {
        let run = || {
            let mut e = Engine::new(Config::default());
            let mut all = Vec::new();
            for i in 0..20u32 {
                let v = [1.0, (i % 3) as f32, (i % 5) as f32 * 0.1, 0.3];
                let (_, ev) = e.process(
                    &input(&format!("id{i}"), &format!("title {i}"), "s", "en", &v),
                    i as i64,
                );
                all.extend(ev);
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
            e.process(&EmbeddedArticle::default(), 0).0,
            Outcome::Invalid
        );
        e.process(&input("a", "t", "s", "en", &[1.0, 0.0]), 1);
        let (o, _) = e.process(&input("b", "t2", "s", "en", &[1.0, 0.0, 0.0]), 2);
        assert_eq!(o, Outcome::Invalid); // dimension mismatch
    }
}
