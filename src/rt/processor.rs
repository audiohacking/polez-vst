//! Block-based overlap-add wrapper for polez sanitization and sliding-window detection.

use super::detect_worker::{DETECT_ANALYSIS_SAMPLES, DetectWorker, POLEZ_MIN_DETECT_SAMPLES};
use ndarray::Array2;
use polez::audio::AudioBuffer;
use polez::config::{AdvancedFlags, FingerprintRemovalConfig, defaults};
use polez::error::Result;
use polez::sanitization::fingerprint::FingerprintRemover;
use polez::sanitization::pipeline::SanitizationMode;

/// Plugin operating mode (mirrors polez CLI `detect` / `clean` / passthrough).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationMode {
    Bypass,
    Detect,
    Clean,
}

/// Sanitization strength when [`OperationMode::Clean`] is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanStrength {
    Fast,
    Standard,
    Preserving,
    Aggressive,
}

impl CleanStrength {
    fn to_polez(self) -> SanitizationMode {
        match self {
            CleanStrength::Fast => SanitizationMode::Fast,
            CleanStrength::Standard => SanitizationMode::Standard,
            CleanStrength::Preserving => SanitizationMode::Preserving,
            CleanStrength::Aggressive => SanitizationMode::Aggressive,
        }
    }
}

/// Overlap-add clean window — 8192 samples (polez-friendly FFT size, ≈170 ms @ 48 kHz).
pub const RT_WINDOW: usize = DETECT_ANALYSIS_SAMPLES;
const RT_OVERLAP: usize = RT_WINDOW / 4;
const RT_HOP: usize = RT_WINDOW - RT_OVERLAP;

/// Exported for latency tests and alignment helpers.
pub const CLEAN_WINDOW_SAMPLES: usize = RT_WINDOW;

/// At most one fingerprint window per host callback (keeps RT CPU bounded).
const MAX_RT_CLEAN_WINDOWS_PER_BLOCK: usize = 1;

const MAX_CHANNELS: usize = 2;

/// Re-run detection at most twice per second (bounded CPU, still responsive meters).
fn detect_analysis_period(sr: u32) -> usize {
    (sr as usize / 2).max(DETECT_ANALYSIS_SAMPLES)
}

/// Fixed-capacity mono ring for detect history (no `push` growth on the audio thread).
struct MonoRing {
    data: Vec<f32>,
    len: usize,
    start: usize,
}

impl MonoRing {
    fn with_capacity(cap: usize) -> Self {
        Self {
            data: vec![0.0; cap],
            len: 0,
            start: 0,
        }
    }

    fn reset(&mut self) {
        self.len = 0;
        self.start = 0;
    }

    fn capacity(&self) -> usize {
        self.data.len()
    }

    fn push_sample(&mut self, sample: f32) {
        if self.len < self.data.len() {
            self.data[self.len] = sample;
            self.len += 1;
        } else {
            self.data[self.start] = sample;
            self.start = (self.start + 1) % self.data.len();
        }
    }

    fn push_mono_from_channels(&mut self, channels: &[&[f32]], num_samples: usize) {
        let ch_count = channels.len();
        for i in 0..num_samples {
            let mono: f32 = channels.iter().map(|ch| ch[i]).sum::<f32>() / ch_count as f32;
            self.push_sample(mono);
        }
    }

    fn contiguous_tail(&self, n: usize, out: &mut [f32]) {
        let n = n.min(self.filled()).min(out.len());
        if n == 0 {
            return;
        }
        let cap = self.data.len();
        let start = if self.len < cap {
            self.len.saturating_sub(n)
        } else {
            (self.start + cap - n) % cap
        };
        for (i, slot) in out[..n].iter_mut().enumerate() {
            *slot = self.data[(start + i) % cap];
        }
    }

    fn filled(&self) -> usize {
        if self.len < self.data.len() {
            self.len
        } else {
            self.data.len()
        }
    }

    fn as_tail_slice(&self) -> &[f32] {
        let filled = self.filled();
        if filled == 0 {
            return &[];
        }
        if self.len < self.data.len() {
            &self.data[..filled]
        } else {
            // Caller must use `contiguous_tail` when the ring has wrapped.
            &[]
        }
    }
}

/// Real-time processor: bypass, analyze (worker), or clean with fixed algorithmic latency.
pub struct RealtimeProcessor {
    sample_rate: u32,
    mode: OperationMode,
    strength: CleanStrength,
    paranoid: bool,
    flags: AdvancedFlags,
    fp_config: FingerprintRemovalConfig,
    fp_config_rt: FingerprintRemovalConfig,

    detect_worker: DetectWorker,
    detect_ring: MonoRing,
    detect_tail_scratch: Vec<f32>,
    detect_samples_since_analysis: usize,
    detect_confidence: f32,
    watermark_count: usize,

    clean_input: [Vec<f32>; MAX_CHANNELS],
    clean_output: [Vec<f32>; MAX_CHANNELS],
    clean_ready: usize,
    polez_window: AudioBuffer,
    overlap_scratch: [Vec<f32>; MAX_CHANNELS],
    clean_input_cap: usize,
}

impl RealtimeProcessor {
    pub fn new(sample_rate: u32, max_channels: usize) -> Self {
        let _ = max_channels;
        let fp_config = FingerprintRemovalConfig {
            statistical_normalization: true,
            temporal_randomization: true,
            phase_randomization: false,
            micro_timing_perturbation: true,
            human_imperfections: false,
        };
        let config = defaults::default_config();

        let mut processor = Self {
            sample_rate,
            mode: OperationMode::Bypass,
            strength: CleanStrength::Standard,
            paranoid: false,
            flags: config.advanced_flags,
            fp_config: fp_config.clone(),
            fp_config_rt: FingerprintRemovalConfig {
                phase_randomization: false,
                human_imperfections: false,
                ..fp_config
            },
            detect_worker: DetectWorker::new(),
            detect_ring: MonoRing::with_capacity(0),
            detect_tail_scratch: Vec::new(),
            detect_samples_since_analysis: 0,
            detect_confidence: 0.0,
            watermark_count: 0,
            clean_input: std::array::from_fn(|_| Vec::new()),
            clean_output: std::array::from_fn(|_| Vec::new()),
            clean_ready: 0,
            polez_window: AudioBuffer::new(Array2::zeros((RT_WINDOW, MAX_CHANNELS)), sample_rate),
            overlap_scratch: std::array::from_fn(|_| Vec::new()),
            clean_input_cap: 0,
        };
        processor.rebuild_capacities(48_000, 512);
        processor
    }

    pub fn mode(&self) -> OperationMode {
        self.mode
    }

    pub fn reset(&mut self, sample_rate: u32, max_channels: usize, max_block_size: usize) {
        let _ = max_channels;
        self.sample_rate = sample_rate;
        self.rebuild_capacities(sample_rate, max_block_size);
        self.flush_stream_state();
        self.detect_worker
            .reset(sample_rate, self.detect_ring.capacity());
        self.refresh_dsp_config();
    }

    fn rebuild_capacities(&mut self, sample_rate: u32, max_block_size: usize) {
        let history_cap = (sample_rate as usize * 2).max(DETECT_ANALYSIS_SAMPLES * 2);
        self.detect_ring = MonoRing::with_capacity(history_cap);
        self.detect_tail_scratch
            .resize(DETECT_ANALYSIS_SAMPLES, 0.0);

        self.clean_input_cap = RT_WINDOW + max_block_size * 4;
        for ch in 0..MAX_CHANNELS {
            self.clean_input[ch].clear();
            self.clean_input[ch].reserve(self.clean_input_cap);
            self.clean_output[ch].clear();
            self.clean_output[ch].reserve(RT_WINDOW * 2);
            self.overlap_scratch[ch].resize(RT_WINDOW, 0.0);
        }

        if self.polez_window.sample_rate != sample_rate
            || self.polez_window.num_samples() != RT_WINDOW
            || self.polez_window.num_channels() != MAX_CHANNELS
        {
            self.polez_window =
                AudioBuffer::new(Array2::zeros((RT_WINDOW, MAX_CHANNELS)), sample_rate);
        }
    }

    pub fn set_mode(&mut self, mode: OperationMode) {
        if self.mode != mode {
            self.mode = mode;
            self.flush_stream_state();
        }
    }

    pub fn set_strength(&mut self, strength: CleanStrength) {
        if self.strength != strength {
            self.strength = strength;
            self.refresh_dsp_config();
        }
    }

    pub fn set_paranoid(&mut self, paranoid: bool) {
        if self.paranoid != paranoid {
            self.paranoid = paranoid;
            self.refresh_dsp_config();
        }
    }

    pub fn latency_samples(&self) -> u32 {
        match self.mode {
            OperationMode::Clean => RT_WINDOW as u32,
            _ => 0,
        }
    }

    pub fn detection_confidence(&self) -> f32 {
        self.detect_confidence
    }

    /// Drain completed detect jobs from the worker (call at the top of each `process`).
    pub fn poll_detect_results(&mut self) {
        if let Some(result) = self.detect_worker.poll_results(self.sample_rate) {
            self.detect_confidence = result.confidence;
            self.watermark_count = result.watermark_count;
        }
    }

    /// Wait for an in-flight detect job (tests / offline render helpers).
    pub fn flush_detect_worker(&mut self) {
        for _ in 0..200 {
            self.detect_worker.unpark();
            self.poll_detect_results();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Ingest host audio for detect mode (read-only — does not modify channels).
    pub fn ingest_detect_block(&mut self, channels: &[&[f32]], num_samples: usize) {
        if num_samples == 0 || channels.is_empty() {
            return;
        }

        self.detect_ring
            .push_mono_from_channels(channels, num_samples);

        self.detect_samples_since_analysis += num_samples;
        let period = detect_analysis_period(self.sample_rate);
        if self.detect_ring.filled() >= DETECT_ANALYSIS_SAMPLES
            && self.detect_samples_since_analysis >= period
        {
            self.detect_samples_since_analysis = 0;
            let tail = self.detect_ring.as_tail_slice();
            if tail.len() >= POLEZ_MIN_DETECT_SAMPLES && tail.len() == self.detect_ring.filled() {
                self.detect_worker.request_analysis(
                    self.sample_rate,
                    tail,
                    DETECT_ANALYSIS_SAMPLES,
                );
            } else {
                self.detect_ring
                    .contiguous_tail(DETECT_ANALYSIS_SAMPLES, &mut self.detect_tail_scratch);
                self.detect_worker.request_analysis(
                    self.sample_rate,
                    &self.detect_tail_scratch,
                    DETECT_ANALYSIS_SAMPLES,
                );
            }
        }
    }

    /// Process one host block in scratch buffers (clean) or via [`Self::ingest_detect_block`].
    pub fn process_block(&mut self, channels: &mut [&mut [f32]]) {
        let num_samples = channels.first().map(|c| c.len()).unwrap_or(0);
        if num_samples == 0 {
            return;
        }

        match self.mode {
            OperationMode::Bypass => {}
            OperationMode::Detect => {
                let inputs: Vec<&[f32]> = channels.iter().map(|c| &c[..]).collect();
                self.ingest_detect_block(&inputs, num_samples);
            }
            OperationMode::Clean => self.process_clean(channels, num_samples),
        }
    }

    fn flush_stream_state(&mut self) {
        for ch in &mut self.clean_input {
            ch.clear();
        }
        for ch in &mut self.clean_output {
            ch.clear();
        }
        self.clean_ready = 0;
        self.detect_ring.reset();
        self.detect_samples_since_analysis = 0;
    }

    fn refresh_dsp_config(&mut self) {
        let config = defaults::default_config();
        self.flags = config.advanced_flags;
        self.fp_config = FingerprintRemovalConfig {
            statistical_normalization: true,
            temporal_randomization: true,
            phase_randomization: self.strength == CleanStrength::Aggressive,
            micro_timing_perturbation: true,
            human_imperfections: self.strength == CleanStrength::Preserving
                || self.strength == CleanStrength::Aggressive,
        };
        self.fp_config_rt = FingerprintRemovalConfig {
            phase_randomization: false,
            human_imperfections: false,
            ..self.fp_config.clone()
        };
    }

    fn clean_window_dsp(
        buffer: &mut AudioBuffer,
        paranoid: bool,
        fp_config_rt: &FingerprintRemovalConfig,
        strength: CleanStrength,
    ) -> Result<()> {
        match strength.to_polez() {
            SanitizationMode::Fast => Ok(()),
            SanitizationMode::Standard => {
                FingerprintRemover::remove(buffer, paranoid, fp_config_rt)
            }
            SanitizationMode::Preserving | SanitizationMode::Aggressive => {
                FingerprintRemover::remove(buffer, paranoid, fp_config_rt)?;
                Ok(())
            }
        }
    }

    fn process_clean(&mut self, channels: &mut [&mut [f32]], num_samples: usize) {
        let ch_count = channels.len().min(MAX_CHANNELS);

        for (ch, samples) in channels.iter().take(ch_count).enumerate() {
            if self.clean_input[ch].len() + samples.len() > self.clean_input_cap {
                let keep = self.clean_input[ch].len().saturating_sub(RT_WINDOW);
                if keep > 0 {
                    self.clean_input[ch].drain(0..keep);
                }
            }
            self.clean_input[ch].extend_from_slice(samples);
        }

        let mut windows_done = 0;
        while self.clean_input[0].len() >= RT_WINDOW
            && windows_done < MAX_RT_CLEAN_WINDOWS_PER_BLOCK
        {
            self.run_clean_window(ch_count);
            for slot in self.clean_input.iter_mut().take(ch_count) {
                slot.drain(0..RT_HOP);
            }
            windows_done += 1;
        }

        let emit = num_samples.min(self.clean_ready);
        if emit == 0 {
            return;
        }

        for (out, ready) in channels
            .iter_mut()
            .take(ch_count)
            .zip(self.clean_output.iter().take(ch_count))
        {
            let copy_len = emit.min(ready.len());
            out[..copy_len].copy_from_slice(&ready[..copy_len]);
        }

        for ready in self.clean_output.iter_mut().take(ch_count) {
            ready.drain(0..emit);
        }
        self.clean_ready = self.clean_ready.saturating_sub(emit);
    }

    fn run_clean_window(&mut self, ch_count: usize) {
        for ch in 0..ch_count {
            let slice = &self.clean_input[ch][..RT_WINDOW];
            for (i, &v) in slice.iter().enumerate() {
                self.polez_window.samples[[i, ch]] = v;
            }
        }

        let original_rms = self.polez_window.rms();
        let _ = Self::clean_window_dsp(
            &mut self.polez_window,
            self.paranoid,
            &self.fp_config_rt,
            self.strength,
        );

        if self.polez_window.rms() > 1e-10 && original_rms > 1e-10 {
            self.polez_window.normalize_rms(original_rms);
        }
        self.polez_window.soft_clip(0.99);

        for ch in 0..ch_count {
            for (i, &v) in self.polez_window.channel(ch).iter().enumerate() {
                self.overlap_scratch[ch][i] = v;
            }
        }
        for ch in 0..ch_count {
            Self::overlap_add_into(
                &mut self.clean_output[ch],
                &self.overlap_scratch[ch][..RT_WINDOW],
            );
        }

        self.clean_ready = self.clean_ready.max(self.clean_output[0].len());
    }

    fn overlap_add_into(out: &mut Vec<f32>, processed: &[f32]) {
        if out.is_empty() {
            out.extend_from_slice(processed);
            return;
        }

        let overlap = RT_OVERLAP.min(out.len()).min(processed.len());
        for i in 0..overlap {
            let fade_out = (overlap - i) as f32 / overlap as f32;
            let fade_in = i as f32 / overlap as f32;
            out[i] = out[i] * fade_out + processed[i] * fade_in;
        }

        let tail_start = overlap;
        let tail_len = processed.len().saturating_sub(overlap);
        if tail_len > 0 {
            if out.len() < tail_start + tail_len {
                out.resize(tail_start + tail_len, 0.0);
            }
            out[tail_start..tail_start + tail_len]
                .copy_from_slice(&processed[tail_start..tail_start + tail_len]);
        }
    }
}

impl Drop for RealtimeProcessor {
    fn drop(&mut self) {
        self.detect_worker.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_leaves_samples_unchanged() {
        let mut rt = RealtimeProcessor::new(48_000, 2);
        rt.set_mode(OperationMode::Bypass);
        let mut l = [0.25f32; 128];
        let mut r = [0.5f32; 128];
        let mut channels: Vec<&mut [f32]> = vec![&mut l, &mut r];
        rt.process_block(&mut channels);
        assert!((l[0] - 0.25).abs() < 1e-6);
        assert!((r[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn detect_fills_confidence_after_window() {
        let mut rt = RealtimeProcessor::new(48_000, 1);
        rt.reset(48_000, 1, 512);
        rt.set_mode(OperationMode::Detect);
        let mut mono = vec![0.0f32; DETECT_ANALYSIS_SAMPLES];
        for (i, s) in mono.iter_mut().enumerate() {
            let t = i as f32 / 48_000.0;
            *s = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3;
        }
        let mut channels: Vec<&mut [f32]> = vec![&mut mono];
        rt.process_block(&mut channels);
        rt.flush_detect_worker();
        assert!(rt.detection_confidence() >= 0.0);
    }

    #[test]
    fn clean_does_not_zero_warmup_block() {
        let mut rt = RealtimeProcessor::new(48_000, 1);
        rt.set_mode(OperationMode::Clean);
        rt.set_strength(CleanStrength::Fast);
        let mut block = [0.5f32; 256];
        let mut channels: Vec<&mut [f32]> = vec![&mut block];
        rt.process_block(&mut channels);
        assert!((block[0] - 0.5).abs() < 1e-6);
    }
}
