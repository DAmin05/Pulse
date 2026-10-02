//! Applying the Kafka logs to Postgres, exactly once.
//!
//! A batch's rows and the next offset of every topic it read are written in
//! one transaction, and the sink resumes from [`read_offsets`]. So each
//! message's effects land exactly once even across crashes, without Kafka
//! consumer commits. Every statement is also idempotent (`ON CONFLICT DO
//! NOTHING`, guarded updates), so re-applying a batch is harmless.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use pgvector::Vector;
use pulse_core::proto::v1::{CloseReason, EmbeddedArticle, StoryEvent, story_event::Kind};
use serde_json::{Value, json};
use tokio_postgres::{Client, Transaction};

use crate::EVENTS_CHANNEL;

/// Embedding dimension of `articles.embedding` / `stories.centroid`.
pub const DIM: usize = 384;

#[derive(Default)]
pub struct Batch {
    /// (offset in articles.embedded, article)
    pub articles: Vec<(i64, EmbeddedArticle)>,
    pub events: Vec<StoryEvent>,
    /// (topic, partition) → next offset to read after this batch.
    pub offsets: BTreeMap<(String, i32), i64>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.articles.is_empty() && self.events.is_empty() && self.offsets.is_empty()
    }
}

pub fn ts(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

fn vector(values: &[f32]) -> Option<Vector> {
    (values.len() == DIM).then(|| Vector::from(values.to_vec()))
}

/// Empties the read model, offsets included: the sink then rebuilds it from
/// the start of both topics.
pub async fn reset(client: &Client) -> Result<()> {
    client
        .batch_execute("TRUNCATE articles, stories, memberships, story_events, sink_offsets")
        .await?;
    Ok(())
}

pub async fn read_offsets(client: &Client) -> Result<BTreeMap<(String, i32), i64>> {
    Ok(client
        .query(
            "SELECT topic, partition, next_offset FROM sink_offsets",
            &[],
        )
        .await?
        .into_iter()
        .map(|r| ((r.get(0), r.get(1)), r.get(2)))
        .collect())
}

pub async fn apply(client: &mut Client, batch: &Batch) -> Result<()> {
    let tx = client.transaction().await?;
    for (offset, article) in &batch.articles {
        insert_article(&tx, *offset, article).await?;
    }
    for event in &batch.events {
        apply_event(&tx, event)
            .await
            .with_context(|| format!("applying event {}", event.event_id))?;
    }
    for ((topic, partition), next) in &batch.offsets {
        tx.execute(
            "INSERT INTO sink_offsets (topic, partition, next_offset) VALUES ($1, $2, $3)
             ON CONFLICT (topic, partition)
             DO UPDATE SET next_offset = GREATEST(sink_offsets.next_offset, EXCLUDED.next_offset)",
            &[topic, partition, next],
        )
        .await?;
    }
    if let Some(last) = batch.events.last() {
        // Delivered on commit only: listeners never see uncommitted events.
        tx.execute(
            "SELECT pg_notify($1, $2)",
            &[
                &EVENTS_CHANNEL,
                &format!("{}:{}", last.input_offset, last.seq),
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn insert_article(tx: &Transaction<'_>, offset: i64, input: &EmbeddedArticle) -> Result<()> {
    let Some(a) = &input.article else {
        return Ok(());
    };
    tx.execute(
        "INSERT INTO articles (id, input_offset, source_id, source_kind, url, title, summary, lang,
                               published_at, fetched_at, event_time_corrected, model_version, embedding)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (id) DO NOTHING",
        &[
            &a.id,
            &offset,
            &a.source_id,
            &(a.source_kind as i16),
            &a.url,
            &a.title,
            &a.summary,
            &a.lang,
            &ts(a.published_at_ms),
            &ts(a.fetched_at_ms),
            &a.event_time_corrected,
            &input.model_version,
            &vector(&input.vector),
        ],
    )
    .await?;
    Ok(())
}

fn close_reason(reason: i32) -> &'static str {
    match CloseReason::try_from(reason).unwrap_or(CloseReason::Unspecified) {
        CloseReason::Idle => "idle",
        CloseReason::Merged => "merged",
        CloseReason::Split => "split",
        CloseReason::Unspecified => "unspecified",
    }
}

/// `(kind, story_id, payload)` for the event log and the live stream.
pub fn event_json(e: &StoryEvent) -> Option<(&'static str, String, Value)> {
    let watermark = (e.watermark_ms > 0).then_some(e.watermark_ms);
    let base = json!({
        "event_id": e.event_id,
        "offset": e.input_offset,
        "seq": e.seq,
        "event_time": e.event_time_ms,
        "watermark": watermark,
    });
    let (kind, story_id, body) = match e.kind.as_ref()? {
        Kind::Created(c) => (
            "created",
            c.story_id.clone(),
            json!({ "story_id": c.story_id, "seed_article_id": c.seed_article_id,
                    "headline": c.headline, "lang": c.lang, "parent_ids": c.parent_story_ids }),
        ),
        Kind::ArticleAdded(a) => (
            "article_added",
            a.story_id.clone(),
            json!({ "story_id": a.story_id, "article_id": a.article_id, "score": a.score,
                    "is_duplicate": a.is_duplicate, "duplicate_of": a.duplicate_of, "late": a.late }),
        ),
        Kind::Updated(u) => (
            "updated",
            u.story_id.clone(),
            json!({ "story_id": u.story_id, "headline": u.headline,
                    "headline_article_id": u.headline_article_id, "article_count": u.article_count,
                    "source_count": u.source_count, "langs": u.langs }),
        ),
        Kind::Split(s) => (
            "split",
            s.parent_story_id.clone(),
            json!({ "story_id": s.parent_story_id,
                    "children": s.children.iter()
                        .map(|c| json!({ "story_id": c.story_id, "article_count": c.article_ids.len() }))
                        .collect::<Vec<_>>() }),
        ),
        Kind::Merged(m) => (
            "merged",
            m.target_story_id.clone(),
            json!({ "story_id": m.target_story_id, "source_ids": m.source_story_ids }),
        ),
        Kind::Closed(c) => (
            "closed",
            c.story_id.clone(),
            json!({ "story_id": c.story_id, "reason": close_reason(c.reason) }),
        ),
    };
    let mut payload = base;
    payload["kind"] = json!(kind);
    if let (Value::Object(p), Value::Object(b)) = (&mut payload, body) {
        p.extend(b);
    }
    Some((kind, story_id, payload))
}

async fn apply_event(tx: &Transaction<'_>, e: &StoryEvent) -> Result<()> {
    let Some((kind, story_id, payload)) = event_json(e) else {
        return Ok(());
    };
    let offset = e.input_offset;
    let at = ts(e.event_time_ms);
    let inserted = tx
        .execute(
            "INSERT INTO story_events (input_offset, seq, event_id, event_time, watermark, kind, story_id, payload)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT DO NOTHING",
            &[
                &offset,
                &(e.seq as i32),
                &e.event_id,
                &at,
                &((e.watermark_ms > 0).then(|| ts(e.watermark_ms))),
                &kind,
                &story_id,
                &payload,
            ],
        )
        .await?;
    if inserted == 0 {
        return Ok(()); // already applied
    }

    match e.kind.as_ref().expect("checked by event_json") {
        Kind::Created(c) => {
            tx.execute(
                "INSERT INTO stories (id, seed_article_id, created_offset, created_at, updated_offset,
                                      updated_at, headline, headline_article_id, lang, langs, parent_ids)
                 VALUES ($1, $2, $3, $4, $3, $4, $5, $2, $6, ARRAY[$6], $7)
                 ON CONFLICT (id) DO NOTHING",
                &[&c.story_id, &c.seed_article_id, &offset, &at, &c.headline, &c.lang, &c.parent_story_ids],
            )
            .await?;
            // A new story's seed has no ArticleAdded of its own. (Split children's
            // members, seed included, are moved by the preceding Split event.)
            tx.execute(
                "INSERT INTO memberships (article_id, story_id, from_offset, added_at)
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                &[&c.seed_article_id, &c.story_id, &offset, &at],
            )
            .await?;
        }
        Kind::ArticleAdded(a) => {
            tx.execute(
                "INSERT INTO memberships (article_id, story_id, from_offset, added_at, score,
                                          is_duplicate, duplicate_of, late)
                 VALUES ($1, $2, $3, $4, $5, $6, NULLIF($7, ''), $8) ON CONFLICT DO NOTHING",
                &[
                    &a.article_id,
                    &a.story_id,
                    &offset,
                    &at,
                    &a.score,
                    &a.is_duplicate,
                    &a.duplicate_of,
                    &a.late,
                ],
            )
            .await?;
        }
        Kind::Updated(u) => {
            tx.execute(
                "UPDATE stories SET headline = $2, headline_article_id = $3, article_count = $4,
                        source_count = $5, langs = $6, centroid = $7, updated_offset = $8, updated_at = $9
                 WHERE id = $1",
                &[
                    &u.story_id,
                    &u.headline,
                    &u.headline_article_id,
                    &(u.article_count as i32),
                    &(u.source_count as i32),
                    &u.langs,
                    &vector(&u.centroid),
                    &offset,
                    &at,
                ],
            )
            .await?;
        }
        Kind::Split(s) => {
            move_members(tx, &s.parent_story_id, offset).await?;
            for child in &s.children {
                copy_members(
                    tx,
                    &s.parent_story_id,
                    &child.story_id,
                    Some(&child.article_ids),
                    offset,
                )
                .await?;
            }
        }
        Kind::Merged(m) => {
            for source in &m.source_story_ids {
                move_members(tx, source, offset).await?;
                copy_members(tx, source, &m.target_story_id, None, offset).await?;
                tx.execute(
                    "UPDATE stories SET merged_into = $2 WHERE id = $1",
                    &[source, &m.target_story_id],
                )
                .await?;
            }
            tx.execute(
                "UPDATE stories SET merged_from = ARRAY(
                     SELECT DISTINCT x FROM unnest(merged_from || $2::text[]) AS x ORDER BY x)
                 WHERE id = $1",
                &[&m.target_story_id, &m.source_story_ids],
            )
            .await?;
        }
        Kind::Closed(c) => {
            tx.execute(
                "UPDATE stories SET closed_offset = $2, closed_at = $3, close_reason = $4
                 WHERE id = $1 AND closed_offset IS NULL",
                &[&c.story_id, &offset, &at, &close_reason(c.reason)],
            )
            .await?;
        }
    }
    Ok(())
}

/// Ends a story's current memberships at `offset`.
async fn move_members(tx: &Transaction<'_>, story: &str, offset: i64) -> Result<()> {
    tx.execute(
        "UPDATE memberships SET to_offset = $2 WHERE story_id = $1 AND to_offset IS NULL",
        &[&story, &offset],
    )
    .await?;
    Ok(())
}

/// Re-opens memberships ended at `offset` in `from` under `to` (optionally only
/// some articles), keeping their per-article flags.
async fn copy_members(
    tx: &Transaction<'_>,
    from: &str,
    to: &str,
    only: Option<&Vec<String>>,
    offset: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO memberships (article_id, story_id, from_offset, added_at, score,
                                  is_duplicate, duplicate_of, late)
         SELECT article_id, $2, $3, added_at, score, is_duplicate, duplicate_of, late
         FROM memberships
         WHERE story_id = $1 AND to_offset = $3 AND ($4::text[] IS NULL OR article_id = ANY($4))
         ON CONFLICT DO NOTHING",
        &[&from, &to, &offset, &only],
    )
    .await?;
    Ok(())
}
