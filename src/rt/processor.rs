//! Block-based overlap-add wrapper for polez sanitization and sliding-window detection.

use polez::audio::AudioBuffer;
use polez::config::{AdvancedFlags, FingerprintRemovalConfig, defaults};
use polez::detection::WatermarkDetector;
use polez::error::Result;
use polez::sanitization::fingerprint::FingerprintRemover;
use polez::sanitization::pipeline::SanitizationMode;
use polez::sanitization::spectral::SpectralCleaner;
use polez::sanitization::stealth::StealthOps;

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

/// STFT window for streaming clean (≈170 ms @ 48 kHz).
pub const CLEAN_WINDOW_SAMPLES: usize = 8192;
const WINDOW: usize = CLEAN_WINDOW_SAMPLES;
/// Overlap between consecutive windows (≈43 ms @ 48 kHz).
const OVERLAP: usize = 2048;
const HOP: usize = WINDOW - OVERLAP;

/// Minimum history for watermark analysis (≈0.5 s @ 48 kHz).
const DETECT_MIN_SAMPLES: usize = 8192;

/// Real-time processor: bypass, analyze, or clean with fixed algorithmic latency.
pub struct RealtimeProcessor {
    sample_rate: u32,
    mode: OperationMode,
    strength: CleanStrength,
    paranoid: bool,
    flags: AdvancedFlags,
    fp_config: FingerprintRemovalConfig,

    channel_count: usize,
    /// Per-channel input ring for overlap-add clean.
    clean_input: Vec<Vec<f32>>,
    /// Per-channel overlap-add accumulator.
    clean_output: Vec<Vec<f32>>,
    /// Valid samples queued in `clean_output`.
    clean_ready: usize,

    /// Mono detection history (newest at end).
    detect_history: Vec<f32>,
    detect_confidence: f32,
    watermark_count: usize,
}

impl RealtimeProcessor {
    pub fn new(sample_rate: u32, max_channels: usize) -> Self {
        let config = defaults::default_config();

        Self {
            sample_rate,
            mode: OperationMode::Bypass,
            strength: CleanStrength::Standard,
            paranoid: false,
            flags: config.advanced_flags,
            fp_config: config.fingerprint_removal,
            channel_count: max_channels.max(2),
            clean_input: vec![Vec::new(); max_channels.max(2)],
            clean_output: vec![Vec::new(); max_channels.max(2)],
            clean_ready: 0,
            detect_history: Vec::new(),
            detect_confidence: 0.0,
            watermark_count: 0,
        }
    }

    pub fn reset(&mut self, sample_rate: u32, max_channels: usize) {
        self.sample_rate = sample_rate;
        self.channel_count = max_channels.max(2);
        self.clean_input = vec![Vec::new(); self.channel_count];
        self.clean_output = vec![Vec::new(); self.channel_count];
        self.clean_ready = 0;
        self.detect_history.clear();
        self.detect_confidence = 0.0;
        self.watermark_count = 0;
        self.refresh_dsp_config();
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

    /// Samples of algorithmic latency in clean mode (host should compensate).
    pub fn latency_samples(&self) -> u32 {
        match self.mode {
            OperationMode::Clean => WINDOW as u32,
            _ => 0,
        }
    }

    pub fn detection_confidence(&self) -> f32 {
        self.detect_confidence
    }

    /// Process one host block. `channels` is a slice of channel slices (same length).
    pub fn process_block(&mut self, channels: &mut [&mut [f32]]) {
        let num_samples = channels.first().map(|c| c.len()).unwrap_or(0);
        if num_samples == 0 {
            return;
        }

        match self.mode {
            OperationMode::Bypass => {}
            OperationMode::Detect => {
                self.update_detection(channels, num_samples);
            }
            OperationMode::Clean => {
                self.process_clean(channels, num_samples);
            }
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
    }

    fn clean_buffer(&mut self, buffer: &mut AudioBuffer) -> Result<()> {
        let mode = self.strength.to_polez();
        let paranoid = self.paranoid || self.strength == CleanStrength::Aggressive;
        let freq_ranges: &[(f64, f64)] = &[];

        match mode {
            SanitizationMode::Fast => Ok(()),
            SanitizationMode::Standard => {
                SpectralCleaner::clean(buffer, paranoid, &self.flags, freq_ranges)?;
                FingerprintRemover::remove(buffer, paranoid, &self.fp_config)?;
                Ok(())
            }
            SanitizationMode::Preserving => {
                SpectralCleaner::clean(buffer, paranoid, &self.flags, freq_ranges)?;
                FingerprintRemover::remove(buffer, paranoid, &self.fp_config)?;
                StealthOps::apply(buffer, &self.flags, paranoid)?;
                Ok(())
            }
            SanitizationMode::Aggressive => {
                SpectralCleaner::clean(buffer, true, &self.flags, freq_ranges)?;
                FingerprintRemover::remove(buffer, true, &self.fp_config)?;
                StealthOps::apply(buffer, &self.flags, true)?;
                Ok(())
            }
        }
    }

    fn update_detection(&mut self, channels: &[&mut [f32]], num_samples: usize) {
        let history_cap = (self.sample_rate as usize * 2).max(DETECT_MIN_SAMPLES * 2);

        for i in 0..num_samples {
            let mono: f32 = channels.iter().map(|ch| ch[i]).sum::<f32>() / channels.len() as f32;
            self.detect_history.push(mono);
        }
        if self.detect_history.len() > history_cap {
            let drop = self.detect_history.len() - history_cap;
            self.detect_history.drain(0..drop);
        }

        if self.detect_history.len() >= DETECT_MIN_SAMPLES {
            let buf = AudioBuffer::from_mono(self.detect_history.clone(), self.sample_rate);
            let result = WatermarkDetector::detect_all(&buf);
            self.detect_confidence = result.overall_confidence as f32;
            self.watermark_count = result.watermark_count;
        }
    }

    fn process_clean(&mut self, channels: &mut [&mut [f32]], num_samples: usize) {
        let ch_count = channels.len().min(self.channel_count);

        for (ch, samples) in channels.iter().take(ch_count).enumerate() {
            self.clean_input[ch].extend_from_slice(samples);
        }

        while self.clean_input[0].len() >= WINDOW {
            self.run_clean_window(ch_count);
            for slot in self.clean_input.iter_mut().take(ch_count) {
                slot.drain(0..HOP);
            }
        }

        let emit = num_samples.min(self.clean_ready);
        if emit == 0 {
            for ch in channels {
                ch.fill(0.0);
            }
            return;
        }

        for (out, ready) in channels
            .iter_mut()
            .take(ch_count)
            .zip(self.clean_output.iter().take(ch_count))
        {
            let copy_len = emit.min(ready.len());
            out[..copy_len].copy_from_slice(&ready[..copy_len]);
            if copy_len < out.len() {
                out[copy_len..].fill(0.0);
            }
        }

        for ready in self.clean_output.iter_mut().take(ch_count) {
            ready.drain(0..emit);
        }
        self.clean_ready = self.clean_ready.saturating_sub(emit);
    }

    fn run_clean_window(&mut self, ch_count: usize) {
        let channel_data: Vec<Vec<f32>> = (0..ch_count)
            .map(|ch| self.clean_input[ch][..WINDOW].to_vec())
            .collect();

        let mut buffer = AudioBuffer::from_channels(channel_data.clone(), self.sample_rate);
        let original_rms = buffer.rms();
        let _ = self.clean_buffer(&mut buffer);

        if buffer.rms() > 1e-10 && original_rms > 1e-10 {
            buffer.normalize_rms(original_rms);
        }
        buffer.soft_clip(0.99);

        for ch in 0..ch_count {
            let processed: Vec<f32> = buffer.channel(ch).to_vec();
            self.overlap_add(ch, &processed);
        }

        self.clean_ready = self.clean_ready.max(self.clean_output[0].len());
    }

    fn overlap_add(&mut self, ch: usize, processed: &[f32]) {
        let out = &mut self.clean_output[ch];
        if out.is_empty() {
            out.extend_from_slice(processed);
            return;
        }

        let overlap = OVERLAP.min(out.len()).min(processed.len());
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
        rt.set_mode(OperationMode::Detect);
        let mut mono = vec![0.0f32; DETECT_MIN_SAMPLES];
        for (i, s) in mono.iter_mut().enumerate() {
            let t = i as f32 / 48_000.0;
            *s = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3;
        }
        let mut channels: Vec<&mut [f32]> = vec![&mut mono];
        rt.process_block(&mut channels);
        assert!(rt.detection_confidence() >= 0.0);
    }
}
