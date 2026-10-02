//! Story Processor: deterministic online clustering of embedded articles into
//! evolving stories, with event-time watermarks and snapshot/restore.

pub mod ann;
pub mod centering;
pub mod engine;
pub mod live;
pub mod minhash;
pub mod snapshot;
