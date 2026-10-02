//! Read queries for the API.
//!
//! Every query takes a log position (`at`: an offset in articles.embedded).
//! "Now" is just the latest position, so live views and time travel share one
//! code path: a story is alive at `at` if it was created at or before it and
//! not closed by then; an article belongs to the story whose membership row is
//! valid at `at`; headlines come from the last event at or before `at`.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use pgvector::Vector;
use serde::Serialize;
use tokio_postgres::GenericClient;

/// Number of activity buckets per story (one per hour, oldest first).
pub const ACTIVITY_BUCKETS: i32 = 24;

#[derive(Debug, Clone, Copy, Default, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    /// Most distinct sources first: what the world is covering.
    #[default]
    Sources,
    /// Most articles first.
    Size,
    /// Most recently changed first.
    Recent,
}

#[derive(Debug, Clone, Default)]
pub struct StoryQuery {
    pub at: i64,
    pub lang: Option<String>,
    pub min_sources: i64,
    pub sort: Sort,
    pub limit: i64,
    /// Restrict to these story ids (search results, graph nodes).
    pub ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoryCard {
    pub id: String,
    pub headline: String,
    pub headline_article_id: String,
    pub lang: String,
    pub langs: Vec<String>,
    pub article_count: i64,
    pub source_count: i64,
    pub top_sources: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub close_reason: Option<String>,
    pub parent_ids: Vec<String>,
    pub merged_from: Vec<String>,
    pub merged_into: Option<String>,
    /// Articles per hour of publication over the 24h before the view's time.
    pub activity: Vec<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArticleView {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub url: String,
    pub source_id: String,
    pub lang: String,
    pub published_at: DateTime<Utc>,
    pub is_duplicate: bool,
    pub late: bool,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoryRef {
    pub id: String,
    pub headline: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoryDetail {
    #[serde(flatten)]
    pub card: StoryCard,
    pub articles: Vec<ArticleView>,
    pub parents: Vec<StoryRef>,
    pub children: Vec<StoryRef>,
    pub merged_from: Vec<StoryRef>,
    pub merged_into: Option<StoryRef>,
    pub events: Vec<serde_json::Value>,
}

/// The latest position the read model has reached.
pub async fn latest_offset(c: &impl GenericClient) -> Result<i64> {
    let row = c
        .query_one("SELECT COALESCE(MAX(input_offset), -1) FROM articles", &[])
        .await?;
    Ok(row.get(0))
}

/// Position corresponding to a time: the last input fetched at or before it.
pub async fn offset_at(c: &impl GenericClient, time: DateTime<Utc>) -> Result<i64> {
    let row = c
        .query_one(
            "SELECT COALESCE(MAX(input_offset), -1) FROM articles WHERE fetched_at <= $1",
            &[&time],
        )
        .await?;
    Ok(row.get(0))
}

/// Pipeline time at a position: when its input was fetched.
pub async fn time_at(c: &impl GenericClient, at: i64) -> Result<Option<DateTime<Utc>>> {
    let row = c
        .query_one(
            "SELECT MAX(fetched_at) FROM articles WHERE input_offset <= $1",
            &[&at],
        )
        .await?;
    Ok(row.get(0))
}

pub async fn stories(c: &impl GenericClient, q: &StoryQuery) -> Result<Vec<StoryCard>> {
    let order = match q.sort {
        Sort::Sources => "source_count DESC, article_count DESC, updated_offset DESC",
        Sort::Size => "article_count DESC, source_count DESC, updated_offset DESC",
        Sort::Recent => "updated_offset DESC",
    };
    let sql = format!(
        "WITH alive AS (
             SELECT * FROM stories
             WHERE created_offset <= $1 AND (closed_offset IS NULL OR closed_offset > $1)
               AND ($5::text[] IS NULL OR id = ANY($5))
         ),
         members AS (
             SELECT m.story_id, a.source_id, a.lang
             FROM memberships m JOIN articles a ON a.id = m.article_id
             WHERE m.from_offset <= $1 AND (m.to_offset IS NULL OR m.to_offset > $1)
               AND m.story_id IN (SELECT id FROM alive)
         ),
         counts AS (
             SELECT story_id, COUNT(*) AS article_count,
                    COUNT(DISTINCT source_id) AS source_count,
                    ARRAY_AGG(DISTINCT lang ORDER BY lang) AS langs
             FROM members GROUP BY story_id
         ),
         ranked AS (
             SELECT alive.*, counts.article_count AS n, counts.source_count AS s,
                    counts.langs AS l,
                    -- Updated-offset as of `at`: the last event at or before it.
                    (SELECT MAX(input_offset) FROM story_events e
                      WHERE e.story_id = alive.id AND e.input_offset <= $1) AS updated_offset_at
             FROM alive JOIN counts ON counts.story_id = alive.id
             WHERE counts.source_count >= $2
               AND ($3::text IS NULL OR $3 = ANY(counts.langs))
         )
         SELECT id, headline_article_id, lang, l, n, s, created_at, updated_at,
                CASE WHEN closed_offset <= $1 THEN closed_at END,
                CASE WHEN closed_offset <= $1 THEN close_reason END,
                parent_ids, merged_from,
                COALESCE((SELECT payload->>'headline' FROM story_events e
                          WHERE e.story_id = ranked.id AND e.kind IN ('created', 'updated')
                            AND e.input_offset <= $1
                          ORDER BY e.input_offset DESC, e.seq DESC LIMIT 1), headline),
                merged_into
         FROM ranked
         ORDER BY {order}
         LIMIT $4",
        order = order
            .replace("source_count", "s")
            .replace("article_count", "n")
            .replace("updated_offset", "updated_offset_at"),
    );
    let rows = c
        .query(&sql, &[&q.at, &q.min_sources, &q.lang, &q.limit, &q.ids])
        .await?;
    let mut cards: Vec<StoryCard> = rows
        .iter()
        .map(|r| StoryCard {
            id: r.get(0),
            headline_article_id: r.get(1),
            lang: r.get(2),
            langs: r.get(3),
            article_count: r.get(4),
            source_count: r.get(5),
            created_at: r.get(6),
            updated_at: r.get(7),
            closed_at: r.get(8),
            close_reason: r.get(9),
            parent_ids: r.get(10),
            merged_from: r.get(11),
            headline: r.get(12),
            merged_into: r.get(13),
            top_sources: Vec::new(),
            activity: Vec::new(),
        })
        .collect();
    enrich(c, q.at, &mut cards).await?;
    Ok(cards)
}

/// Adds top sources and hourly activity for each card.
async fn enrich(c: &impl GenericClient, at: i64, cards: &mut [StoryCard]) -> Result<()> {
    if cards.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = cards.iter().map(|c| c.id.clone()).collect();
    let reference = time_at(c, at).await?.unwrap_or_else(Utc::now);

    let mut top: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in c
        .query(
            "SELECT story_id, source_id FROM (
                 SELECT m.story_id, a.source_id, COUNT(*) AS n,
                        ROW_NUMBER() OVER (PARTITION BY m.story_id ORDER BY COUNT(*) DESC, a.source_id) AS rank
                 FROM memberships m JOIN articles a ON a.id = m.article_id
                 WHERE m.story_id = ANY($1) AND m.from_offset <= $2
                   AND (m.to_offset IS NULL OR m.to_offset > $2)
                 GROUP BY m.story_id, a.source_id
             ) t WHERE rank <= 3 ORDER BY story_id, rank",
            &[&ids, &at],
        )
        .await?
    {
        top.entry(r.get(0)).or_default().push(r.get(1));
    }

    let mut activity: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for r in c
        .query(
            // By publication time: a story's timeline is when it was written about.
            "SELECT m.story_id,
                    FLOOR(EXTRACT(EPOCH FROM ($3 - a.published_at)) / 3600)::int AS hours_ago,
                    COUNT(*)
             FROM memberships m JOIN articles a ON a.id = m.article_id
             WHERE m.story_id = ANY($1) AND m.from_offset <= $2
               AND (m.to_offset IS NULL OR m.to_offset > $2)
               AND a.published_at <= $3 AND a.published_at > $3 - INTERVAL '24 hours'
             GROUP BY 1, 2",
            &[&ids, &at, &reference],
        )
        .await?
    {
        let hours_ago: i32 = r.get(1);
        let buckets = activity
            .entry(r.get(0))
            .or_insert_with(|| vec![0; ACTIVITY_BUCKETS as usize]);
        if (0..ACTIVITY_BUCKETS).contains(&hours_ago) {
            buckets[(ACTIVITY_BUCKETS - 1 - hours_ago) as usize] += r.get::<_, i64>(2);
        }
    }

    for card in cards {
        card.top_sources = top.remove(&card.id).unwrap_or_default();
        card.activity = activity
            .remove(&card.id)
            .unwrap_or_else(|| vec![0; ACTIVITY_BUCKETS as usize]);
    }
    Ok(())
}

async fn refs(c: &impl GenericClient, ids: &[String]) -> Result<Vec<StoryRef>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(c.query(
        "SELECT id, headline FROM stories WHERE id = ANY($1) ORDER BY created_offset",
        &[&ids],
    )
    .await?
    .iter()
    .map(|r| StoryRef {
        id: r.get(0),
        headline: r.get(1),
    })
    .collect())
}

/// A story as of `at`, with its articles, lineage and event history. Closed
/// stories are returned too (their articles as of just before closing).
pub async fn story(c: &impl GenericClient, id: &str, at: i64) -> Result<Option<StoryDetail>> {
    let Some(row) = c
        .query_opt(
            "SELECT created_offset, closed_offset FROM stories WHERE id = $1 AND created_offset <= $2",
            &[&id, &at],
        )
        .await?
    else {
        return Ok(None);
    };
    let closed: Option<i64> = row.get(1);
    // A closed story's members are the ones valid just before it closed.
    let view_at = match closed {
        Some(closed) if closed <= at => closed - 1,
        _ => at,
    };
    let mut cards = stories(
        c,
        &StoryQuery {
            at: view_at,
            limit: 1,
            ids: Some(vec![id.to_owned()]),
            ..StoryQuery::default()
        },
    )
    .await?;
    let Some(mut card) = cards.pop() else {
        return Ok(None);
    };
    if closed.is_some_and(|o| o <= at) {
        let r = c
            .query_one(
                "SELECT closed_at, close_reason, merged_into FROM stories WHERE id = $1",
                &[&id],
            )
            .await?;
        card.closed_at = r.get(0);
        card.close_reason = r.get(1);
        card.merged_into = r.get(2);
    }

    let articles = c
        .query(
            "SELECT a.id, a.title, a.summary, a.url, a.source_id, a.lang, a.published_at,
                    m.is_duplicate, m.late, m.score
             FROM memberships m JOIN articles a ON a.id = m.article_id
             WHERE m.story_id = $1 AND m.from_offset <= $2
               AND (m.to_offset IS NULL OR m.to_offset > $2)
             ORDER BY a.published_at DESC",
            &[&id, &view_at],
        )
        .await?
        .iter()
        .map(|r| ArticleView {
            id: r.get(0),
            title: r.get(1),
            summary: r.get(2),
            url: r.get(3),
            source_id: r.get(4),
            lang: r.get(5),
            published_at: r.get(6),
            is_duplicate: r.get(7),
            late: r.get(8),
            score: r.get(9),
        })
        .collect();

    let children: Vec<String> = c
        .query(
            "SELECT id FROM stories WHERE $1 = ANY(parent_ids) AND created_offset <= $2",
            &[&id, &at],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    let events = c
        .query(
            "SELECT payload FROM story_events
             WHERE (story_id = $1 OR payload->'source_ids' ? $1) AND input_offset <= $2
             ORDER BY input_offset DESC, seq DESC LIMIT 200",
            &[&id, &at],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();

    Ok(Some(StoryDetail {
        parents: refs(c, &card.parent_ids).await?,
        children: refs(c, &children).await?,
        merged_from: refs(c, &card.merged_from).await?,
        merged_into: match &card.merged_into {
            Some(target) => refs(c, std::slice::from_ref(target)).await?.pop(),
            None => None,
        },
        card,
        articles,
        events,
    }))
}

/// Centered-space centroids for similarity edges between stories.
pub async fn centroids(c: &impl GenericClient, ids: &[String]) -> Result<Vec<(String, Vec<f32>)>> {
    Ok(c.query(
        "SELECT id, centroid FROM stories WHERE id = ANY($1) AND centroid IS NOT NULL",
        &[&ids],
    )
    .await?
    .iter()
    .map(|r| (r.get(0), r.get::<_, Vector>(1).to_vec()))
    .collect())
}

/// Recent split/merge events at or before `at`, newest first.
pub async fn lineage_events(
    c: &impl GenericClient,
    at: i64,
    limit: i64,
) -> Result<Vec<serde_json::Value>> {
    Ok(c.query(
        "SELECT payload FROM story_events
             WHERE kind IN ('split', 'merged') AND input_offset <= $1
             ORDER BY input_offset DESC, seq DESC LIMIT $2",
        &[&at, &limit],
    )
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect())
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub article: ArticleView,
    pub similarity: f32,
    pub story_id: Option<String>,
}

/// Nearest articles to an e5 query vector (cross-lingual), with their current story.
pub async fn search(c: &impl GenericClient, query: Vec<f32>, limit: i64) -> Result<Vec<SearchHit>> {
    let rows = c
        .query(
            "SELECT a.id, a.title, a.summary, a.url, a.source_id, a.lang, a.published_at,
                    1 - (a.embedding <=> $1) AS similarity, m.story_id,
                    COALESCE(m.is_duplicate, false), COALESCE(m.late, false), COALESCE(m.score, 0)
             FROM (SELECT * FROM articles WHERE embedding IS NOT NULL
                   ORDER BY embedding <=> $1 LIMIT $2) a
             LEFT JOIN memberships m ON m.article_id = a.id AND m.to_offset IS NULL
             ORDER BY similarity DESC",
            &[&Vector::from(query), &limit],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| SearchHit {
            article: ArticleView {
                id: r.get(0),
                title: r.get(1),
                summary: r.get(2),
                url: r.get(3),
                source_id: r.get(4),
                lang: r.get(5),
                published_at: r.get(6),
                is_duplicate: r.get(9),
                late: r.get(10),
                score: r.get(11),
            },
            similarity: r.get::<_, f64>(7) as f32,
            story_id: r.get(8),
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct TimelineBucket {
    pub start: DateTime<Utc>,
    /// Last position in the bucket (for time travel).
    pub offset: i64,
    pub articles: i64,
    pub created: i64,
    pub splits: i64,
    pub merges: i64,
    pub closed: i64,
}

/// Activity over the whole history in equal buckets of pipeline time.
pub async fn timeline(c: &impl GenericClient, buckets: i32) -> Result<Vec<TimelineBucket>> {
    let rows = c
        .query(
            "WITH bounds AS (
                 SELECT MIN(fetched_at) AS lo,
                        GREATEST(MAX(fetched_at), MIN(fetched_at) + INTERVAL '1 second') AS hi
                 FROM articles
             ),
             a AS (
                 SELECT LEAST(WIDTH_BUCKET(EXTRACT(EPOCH FROM fetched_at),
                                           EXTRACT(EPOCH FROM lo), EXTRACT(EPOCH FROM hi), $1), $1) AS b,
                        input_offset
                 FROM articles, bounds
             ),
             e AS (
                 SELECT a.b, ev.kind FROM story_events ev JOIN a ON a.input_offset = ev.input_offset
             )
             SELECT a.b, MAX(a.input_offset), COUNT(*),
                    (SELECT COUNT(*) FROM e WHERE e.b = a.b AND e.kind = 'created'),
                    (SELECT COUNT(*) FROM e WHERE e.b = a.b AND e.kind = 'split'),
                    (SELECT COUNT(*) FROM e WHERE e.b = a.b AND e.kind = 'merged'),
                    (SELECT COUNT(*) FROM e WHERE e.b = a.b AND e.kind = 'closed'),
                    (SELECT lo + (hi - lo) * (a.b - 1) / $1 FROM bounds)
             FROM a GROUP BY a.b ORDER BY a.b",
            &[&buckets],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| TimelineBucket {
            offset: r.get(1),
            articles: r.get(2),
            created: r.get(3),
            splits: r.get(4),
            merges: r.get(5),
            closed: r.get(6),
            start: r.get(7),
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct Totals {
    pub articles: i64,
    pub stories_open: i64,
    pub stories_total: i64,
    pub languages: i64,
    pub sources: i64,
    pub splits: i64,
    pub merges: i64,
    pub late_articles: i64,
    pub duplicates: i64,
    pub first_fetched_at: Option<DateTime<Utc>>,
    pub last_fetched_at: Option<DateTime<Utc>>,
    pub latest_offset: i64,
}

pub async fn totals(c: &impl GenericClient) -> Result<Totals> {
    let r = c
        .query_one(
            "SELECT
                 (SELECT COUNT(*) FROM articles),
                 (SELECT COUNT(*) FROM stories WHERE closed_offset IS NULL),
                 (SELECT COUNT(*) FROM stories),
                 (SELECT COUNT(DISTINCT lang) FROM articles),
                 (SELECT COUNT(DISTINCT source_id) FROM articles),
                 (SELECT COUNT(*) FROM story_events WHERE kind = 'split'),
                 (SELECT COUNT(*) FROM story_events WHERE kind = 'merged'),
                 (SELECT COUNT(*) FROM memberships WHERE late AND to_offset IS NULL),
                 (SELECT COUNT(*) FROM memberships WHERE is_duplicate AND to_offset IS NULL),
                 (SELECT MIN(fetched_at) FROM articles),
                 (SELECT MAX(fetched_at) FROM articles),
                 (SELECT COALESCE(MAX(input_offset), -1) FROM articles)",
            &[],
        )
        .await?;
    Ok(Totals {
        articles: r.get(0),
        stories_open: r.get(1),
        stories_total: r.get(2),
        languages: r.get(3),
        sources: r.get(4),
        splits: r.get(5),
        merges: r.get(6),
        late_articles: r.get(7),
        duplicates: r.get(8),
        first_fetched_at: r.get(9),
        last_fetched_at: r.get(10),
        latest_offset: r.get(11),
    })
}

/// Story events strictly after (`offset`, `seq`), oldest first: the live stream's feed.
pub async fn events_after(
    c: &impl GenericClient,
    offset: i64,
    seq: i32,
    limit: i64,
) -> Result<Vec<((i64, i32), serde_json::Value)>> {
    Ok(c.query(
        "SELECT input_offset, seq, payload FROM story_events
             WHERE (input_offset, seq) > ($1, $2)
             ORDER BY input_offset, seq LIMIT $3",
        &[&offset, &seq, &limit],
    )
    .await?
    .iter()
    .map(|r| ((r.get(0), r.get(1)), r.get(2)))
    .collect())
}
