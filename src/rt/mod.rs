//! Real-time adapters around the offline [polez](https://github.com/szichedelic/polez) engine.

mod processor;

pub use processor::{CleanStrength, OperationMode, RealtimeProcessor};
pub use processor::CLEAN_WINDOW_SAMPLES;
