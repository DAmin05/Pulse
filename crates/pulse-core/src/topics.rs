//! Kafka topic names and their required layout.
//!
//! `deploy/redpanda/create-topics.sh` creates these; `pulse doctor` verifies them.

/// Raw articles from the Ingestor. Keyed by article id.
pub const ARTICLES_RAW: &str = "articles.raw";
/// Articles with embeddings. Single partition: its log order is the processing
/// order, which is what makes the Story Processor deterministic.
pub const ARTICLES_EMBEDDED: &str = "articles.embedded";
/// Articles that arrived behind the watermark by more than the allowed lateness.
pub const ARTICLES_LATE: &str = "articles.late";
/// Story graph changes from the Story Processor.
pub const STORIES_EVENTS: &str = "stories.events";

/// Prefix for isolated replay output topics.
pub const REPLAY_PREFIX: &str = "replay.";

pub struct TopicSpec {
    pub name: &'static str,
    pub partitions: i32,
}

/// Topics that must exist before any service starts.
pub const REQUIRED: &[TopicSpec] = &[
    TopicSpec {
        name: ARTICLES_RAW,
        partitions: 3,
    },
    TopicSpec {
        name: ARTICLES_EMBEDDED,
        partitions: 1,
    },
    TopicSpec {
        name: ARTICLES_LATE,
        partitions: 1,
    },
    TopicSpec {
        name: STORIES_EVENTS,
        partitions: 1,
    },
];

/// Output topic for a replay run.
pub fn replay_topic(run_id: &str) -> String {
    format!("{REPLAY_PREFIX}{run_id}.stories")
}
