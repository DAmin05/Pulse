//! Read model against a real Postgres: event application, idempotency and
//! time travel through a merge and a split.
//!
//! Runs when PULSE_TEST_DATABASE_URL is set (CI's stack job and `make test-db`);
//! each run uses its own schema, so it never touches the live read model.

use std::collections::BTreeMap;

use pulse_core::proto::v1::{
    Article, ArticleAdded, CloseReason, EmbeddedArticle, SplitChild, StoryClosed, StoryCreated,
    StoryEvent, StoryMerged, StorySplit, story_event::Kind,
};
use pulse_store::reader::{self, StoryQuery};
use pulse_store::writer::{self, Batch};

/// A client on a fresh schema; the schema is dropped when the guard drops.
struct TestDb {
    client: pulse_store::tokio_postgres::Client,
    schema: String,
    url: String,
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let (url, schema) = (self.url.clone(), self.schema.clone());
        // Separate runtime: Drop can't await, and the test's runtime is shutting down.
        let _ = std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                if let Ok(c) = pulse_store::connect(&url).await {
                    let _ = c
                        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
                        .await;
                }
            })
        })
        .join();
    }
}

impl std::ops::Deref for TestDb {
    type Target = pulse_store::tokio_postgres::Client;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl std::ops::DerefMut for TestDb {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.client
    }
}

async fn client() -> Option<TestDb> {
    let url = std::env::var("PULSE_TEST_DATABASE_URL").ok()?;
    let mut c = pulse_store::connect(&url).await.expect("connect");
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let schema = format!("test_{}_{}_{n}", std::process::id(), rand_suffix());
    c.batch_execute(&format!(
        "CREATE SCHEMA {schema}; SET search_path TO {schema}, public"
    ))
    .await
    .unwrap();
    pulse_store::migrate(&mut c).await.unwrap();
    Some(TestDb {
        client: c,
        schema,
        url,
    })
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn article(id: &str, offset: i64) -> (i64, EmbeddedArticle) {
    (
        offset,
        EmbeddedArticle {
            article: Some(Article {
                id: id.into(),
                title: format!("title {id}"),
                source_id: format!("src-{id}"),
                lang: "en".into(),
                url: format!("https://example.com/{id}"),
                published_at_ms: 1_000_000 + offset * 1000,
                fetched_at_ms: 2_000_000 + offset * 1000,
                ..Default::default()
            }),
            vector: vec![0.0; 384],
            model_version: "t".into(),
        },
    )
}

fn event(offset: i64, seq: u32, kind: Kind) -> StoryEvent {
    StoryEvent {
        event_id: format!("e{offset}-{seq}"),
        event_time_ms: 1_000_000 + offset * 1000,
        watermark_ms: 0,
        input_offset: offset,
        seq,
        kind: Some(kind),
    }
}

fn created(story: &str, seed: &str, parents: &[&str]) -> Kind {
    Kind::Created(StoryCreated {
        story_id: story.into(),
        seed_article_id: seed.into(),
        headline: format!("headline {story}"),
        lang: "en".into(),
        parent_story_ids: parents.iter().map(|p| p.to_string()).collect(),
    })
}

fn closed(story: &str, reason: CloseReason) -> Kind {
    Kind::Closed(StoryClosed {
        story_id: story.into(),
        reason: reason as i32,
    })
}

/// offset 0: s1 created (a1)        offset 1: a2 joins s1
/// offset 2: s2 created (a3)        offset 3: s2 merges into s1
/// offset 4: s1 splits into c1 {a1, a3} and c2 {a2}
fn history() -> Batch {
    let events = vec![
        event(0, 0, created("s1", "a1", &[])),
        event(
            1,
            0,
            Kind::ArticleAdded(ArticleAdded {
                story_id: "s1".into(),
                article_id: "a2".into(),
                score: 0.9,
                ..Default::default()
            }),
        ),
        event(2, 0, created("s2", "a3", &[])),
        event(
            3,
            0,
            Kind::Merged(StoryMerged {
                source_story_ids: vec!["s2".into()],
                target_story_id: "s1".into(),
            }),
        ),
        event(3, 1, closed("s2", CloseReason::Merged)),
        event(
            4,
            0,
            Kind::Split(StorySplit {
                parent_story_id: "s1".into(),
                children: vec![
                    SplitChild {
                        story_id: "c1".into(),
                        article_ids: vec!["a1".into(), "a3".into()],
                    },
                    SplitChild {
                        story_id: "c2".into(),
                        article_ids: vec!["a2".into()],
                    },
                ],
            }),
        ),
        event(4, 1, created("c1", "a1", &["s1"])),
        event(4, 2, created("c2", "a2", &["s1"])),
        event(4, 3, closed("s1", CloseReason::Split)),
    ];
    Batch {
        articles: vec![
            article("a1", 0),
            article("a2", 1),
            article("a3", 2),
            article("a4", 4),
        ],
        events,
        offsets: BTreeMap::from([(("stories.events".into(), 0), 9)]),
    }
}

/// Story id → article count at `at`.
async fn graph_at(c: &pulse_store::tokio_postgres::Client, at: i64) -> BTreeMap<String, i64> {
    reader::stories(
        c,
        &StoryQuery {
            at,
            limit: 100,
            ..StoryQuery::default()
        },
    )
    .await
    .unwrap()
    .into_iter()
    .map(|s| (s.id, s.article_count))
    .collect()
}

type Snapshot = (i64, i64, i64, i64, String);

/// Row counts plus every membership row: any change shows up.
async fn snapshot(c: &pulse_store::tokio_postgres::Client) -> Snapshot {
    let r = c
        .query_one(
            "SELECT (SELECT COUNT(*) FROM articles), (SELECT COUNT(*) FROM stories),
                    (SELECT COUNT(*) FROM memberships), (SELECT COUNT(*) FROM story_events),
                    (SELECT string_agg(article_id || story_id || from_offset || coalesce(to_offset::text, '-'), ','
                            ORDER BY article_id, from_offset) FROM memberships)",
            &[],
        )
        .await
        .unwrap();
    (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4))
}

fn map(pairs: &[(&str, i64)]) -> BTreeMap<String, i64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[tokio::test]
async fn time_travel_through_merge_and_split_is_idempotent() {
    let Some(mut c) = client().await else {
        eprintln!("PULSE_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let batch = history();
    writer::apply(&mut c.client, &batch).await.unwrap();

    let expected = [
        (0, map(&[("s1", 1)])),
        (1, map(&[("s1", 2)])),
        (2, map(&[("s1", 2), ("s2", 1)])),
        (3, map(&[("s1", 3)])),
        (4, map(&[("c1", 2), ("c2", 1)])),
    ];
    for (at, want) in &expected {
        assert_eq!(&graph_at(&c.client, *at).await, want, "as of offset {at}");
    }

    // Lineage and closed-story views.
    let c1 = reader::story(&c.client, "c1", 4).await.unwrap().unwrap();
    assert_eq!(
        c1.parents.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["s1"]
    );
    let s1 = reader::story(&c.client, "s1", 4).await.unwrap().unwrap();
    assert_eq!(s1.card.close_reason.as_deref(), Some("split"));
    assert_eq!(
        s1.articles.len(),
        3,
        "a closed story shows its members as of closing"
    );
    assert_eq!(
        s1.merged_from
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["s2"]
    );
    let mut kids: Vec<_> = s1.children.iter().map(|p| p.id.as_str()).collect();
    kids.sort_unstable();
    assert_eq!(kids, ["c1", "c2"]);
    let s2 = reader::story(&c.client, "s2", 4).await.unwrap().unwrap();
    assert_eq!(s2.merged_into.map(|r| r.id).as_deref(), Some("s1"));

    // Applying everything again changes nothing.
    let before = snapshot(&c.client).await;
    writer::apply(&mut c.client, &batch).await.unwrap();
    assert_eq!(snapshot(&c.client).await, before);
    for (at, want) in &expected {
        assert_eq!(
            &graph_at(&c.client, *at).await,
            want,
            "as of offset {at} after re-apply"
        );
    }

    // Offsets only move forward; the live feed reads events in order.
    let offsets = writer::read_offsets(&c.client).await.unwrap();
    assert_eq!(offsets[&("stories.events".to_string(), 0)], 9);
    let after = reader::events_after(&c.client, 3, 0, 100).await.unwrap();
    let positions: Vec<_> = after.iter().map(|(p, _)| *p).collect();
    assert_eq!(positions, [(3, 1), (4, 0), (4, 1), (4, 2), (4, 3)]);
    assert_eq!(after[0].1["kind"], "closed");
    assert_eq!(after[0].1["reason"], "merged");
}

#[tokio::test]
async fn time_maps_to_position() {
    let Some(mut c) = client().await else {
        return;
    };
    writer::apply(&mut c.client, &history()).await.unwrap();
    // fetched_at = 2_000_000 + offset * 1000 ms.
    let at = |ms: i64| writer::ts(ms);
    assert_eq!(
        reader::offset_at(&c.client, at(2_002_500)).await.unwrap(),
        2
    );
    assert_eq!(reader::offset_at(&c.client, at(1_000)).await.unwrap(), -1);
    assert_eq!(reader::latest_offset(&c.client).await.unwrap(), 4);
    let t = reader::totals(&c.client).await.unwrap();
    assert_eq!(
        (t.articles, t.stories_open, t.merges, t.splits),
        (4, 2, 1, 1)
    );
}
