//! Real-time adapters around the offline [polez](https://github.com/szichedelic/polez) engine.

mod detect_worker;
mod processor;

pub use detect_worker::{DETECT_ANALYSIS_SAMPLES, POLEZ_MIN_DETECT_SAMPLES};
pub use processor::CLEAN_WINDOW_SAMPLES;
pub use processor::{CleanStrength, OperationMode, RealtimeProcessor};
