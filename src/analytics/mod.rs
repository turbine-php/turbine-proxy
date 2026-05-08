//! Query analytics — collects latency, throughput, and slow query data.

pub mod advisor;
pub mod collector;
pub mod storage;
pub mod timeseries;

pub use collector::Collector;
pub use storage::AnalyticsStorage;
pub use timeseries::{ThroughputCounters, TimeseriesStore};
