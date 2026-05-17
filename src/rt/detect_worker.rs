//! Background watermark analysis (Truce worker-pattern).
//!
//! [`WatermarkDetector::detect_all`] runs on a dedicated thread so the
//! audio thread never pays for rayon / parallel method evaluation.
//! See [truce-example-fundsp-reverb-worker](https://github.com/truce-audio/truce/tree/main/examples/truce-example-fundsp-reverb-worker).

use crossbeam_queue::ArrayQueue;
use polez::audio::AudioBuffer;
use polez::detection::WatermarkDetector;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::{self, JoinHandle, Thread};

/// Polez returns immediately when `num_samples < 4096` (`watermark.rs`).
pub const POLEZ_MIN_DETECT_SAMPLES: usize = 4096;

/// Analysis window passed to `detect_all` (2× polez minimum for stable scores).
pub const DETECT_ANALYSIS_SAMPLES: usize = 8192;

#[derive(Copy, Clone)]
struct DetectRequest {
    sample_rate: u32,
    sample_rate_bits: u64,
    buffer_idx: u8,
    len: usize,
}

pub struct DetectResult {
    pub sample_rate_bits: u64,
    pub confidence: f32,
    pub watermark_count: usize,
}

struct DetectChannel {
    /// Double-buffered mono snapshots shared between audio and worker threads.
    buffers: Mutex<[Vec<f32>; 2]>,
    requests: ArrayQueue<DetectRequest>,
    ready: ArrayQueue<DetectResult>,
    shutdown: AtomicBool,
}

/// Lock-free detect handoff (same shape as fundsp reverb worker queues).
pub struct DetectWorker {
    channel: Arc<DetectChannel>,
    worker_thread: Thread,
    worker_handle: Option<JoinHandle<()>>,
    next_write_idx: u8,
}

impl DetectWorker {
    pub fn new() -> Self {
        let channel = Arc::new(DetectChannel {
            buffers: Mutex::new([Vec::new(), Vec::new()]),
            requests: ArrayQueue::new(1),
            ready: ArrayQueue::new(1),
            shutdown: AtomicBool::new(false),
        });

        let worker_handle = spawn_detect_worker(Arc::clone(&channel));
        let worker_thread = worker_handle.thread().clone();

        Self {
            channel,
            worker_thread,
            worker_handle: Some(worker_handle),
            next_write_idx: 0,
        }
    }

    pub fn reset(&mut self, sample_rate: u32, history_cap: usize) {
        let cap = history_cap.max(DETECT_ANALYSIS_SAMPLES);
        let mut buffers = self.channel.buffers.lock().expect("detect buffers");
        for buf in &mut *buffers {
            buf.resize(cap, 0.0);
        }
        drop(buffers);
        while self.channel.requests.pop().is_some() {}
        while self.channel.ready.pop().is_some() {}
        self.next_write_idx = 0;
        let _ = sample_rate;
    }

    /// Copy the latest `len` mono samples into the inactive buffer and queue analysis.
    pub fn request_analysis(&mut self, sample_rate: u32, mono_tail: &[f32], len: usize) {
        let len = len.min(mono_tail.len()).min(DETECT_ANALYSIS_SAMPLES);
        if len < POLEZ_MIN_DETECT_SAMPLES {
            return;
        }

        let idx = self.next_write_idx;
        self.next_write_idx ^= 1;

        {
            let mut buffers = self.channel.buffers.lock().expect("detect buffers");
            let dst = &mut buffers[idx as usize];
            if dst.len() < len {
                dst.resize(len, 0.0);
            }
            let start = mono_tail.len() - len;
            dst[..len].copy_from_slice(&mono_tail[start..start + len]);
        }

        self.channel.requests.force_push(DetectRequest {
            sample_rate,
            sample_rate_bits: f64::from(sample_rate).to_bits(),
            buffer_idx: idx,
            len,
        });
        self.worker_thread.unpark();
    }

    /// Apply the newest finished analysis if it matches `sample_rate`.
    pub fn poll_results(&self, sample_rate: u32) -> Option<DetectResult> {
        let sr_bits = f64::from(sample_rate).to_bits();
        if let Some(result) = self.channel.ready.pop() {
            if result.sample_rate_bits == sr_bits {
                return Some(result);
            }
        }
        None
    }

    pub fn unpark(&self) {
        self.worker_thread.unpark();
    }

    pub fn shutdown(&self) {
        self.channel.shutdown.store(true, Ordering::Release);
        self.worker_thread.unpark();
    }
}

impl Drop for DetectWorker {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_detect_worker(channel: Arc<DetectChannel>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("polez-detect".into())
        .spawn(move || {
            loop {
                let mut latest: Option<DetectRequest> = None;
                while let Some(req) = channel.requests.pop() {
                    latest = Some(req);
                }

                if let Some(req) = latest {
                    let buffers = channel.buffers.lock().expect("detect buffers");
                    let slice = &buffers[req.buffer_idx as usize][..req.len];
                    let buf = AudioBuffer::from_mono(slice.to_vec(), req.sample_rate);
                    drop(buffers);
                    let result = WatermarkDetector::detect_all(&buf);
                    let _ = channel.ready.force_push(DetectResult {
                        sample_rate_bits: req.sample_rate_bits,
                        confidence: result.overall_confidence as f32,
                        watermark_count: result.watermark_count,
                    });
                }

                if channel.shutdown.load(Ordering::Acquire) {
                    return;
                }
                thread::park();
            }
        })
        .expect("spawn polez-detect worker")
}
