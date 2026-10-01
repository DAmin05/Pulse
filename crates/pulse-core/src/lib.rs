//! Shared building blocks for every Pulse service.

pub mod config;
pub mod ids;
pub mod kafka;
pub mod telemetry;
pub mod topics;

/// Generated protobuf types (see `proto/pulse/v1`).
pub mod proto {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/pulse.v1.rs"));
    }
}
