//! Shared fixtures and reference (offline) processing for integration tests.
//!
//! Mirrors helpers in `polez::sanitization::pipeline` tests so realtime output can
//! be compared against the same expectations.

use polez::audio::AudioBuffer;
use polez::config::{FingerprintRemovalConfig, defaults};
use polez::detection::WatermarkDetector;
use polez::sanitization::fingerprint::FingerprintRemover;
use polez::sanitization::pipeline::SanitizationMode;
use polez::sanitization::spectral::SpectralCleaner;
use polez::sanitization::stealth::StealthOps;
use polez_vst::rt::{CLEAN_WINDOW_SAMPLES, CleanStrength, OperationMode, RealtimeProcessor};

pub const TEST_SR: u32 = 44_100;
pub const BLOCK_SIZE: usize = 512;

pub fn sine_mono(freq: f32, sr: u32, duration_secs: f32) -> Vec<f32> {
    let len = (sr as f32 * duration_secs) as usize;
    (0..len)
        .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin() * 0.5)
        .collect()
}

/// Watermarked synthetic signal (from polez `sanitization::pipeline` tests).
pub fn watermarked_mono(sr: u32) -> Vec<f32> {
    let len = sr as usize;
    (0..len)
        .map(|i| {
            let t = i as f32 / sr as f32;
            let base = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.4;
            let wm1 = (2.0 * std::f32::consts::PI * 18500.0 * t).sin() * 0.05;
            let wm2 = (2.0 * std::f32::consts::PI * 19500.0 * t).sin() * 0.04;
            let wm3 = (2.0 * std::f32::consts::PI * 20500.0 * t).sin() * 0.03;
            let mod_factor = 1.0 + 0.02 * (2.0 * std::f32::consts::PI * 50.0 * t).sin();
            (base + wm1 + wm2 + wm3) * mod_factor
        })
        .collect()
}

pub fn detect_confidence(samples: &[f32], sr: u32) -> f64 {
    let buf = AudioBuffer::from_mono(samples.to_vec(), sr);
    WatermarkDetector::detect_all(&buf).overall_confidence
}

fn strength_to_mode(strength: CleanStrength) -> SanitizationMode {
    match strength {
        CleanStrength::Fast => SanitizationMode::Fast,
        CleanStrength::Standard => SanitizationMode::Standard,
        CleanStrength::Preserving => SanitizationMode::Preserving,
        CleanStrength::Aggressive => SanitizationMode::Aggressive,
    }
}

fn fp_config(strength: CleanStrength) -> FingerprintRemovalConfig {
    FingerprintRemovalConfig {
        statistical_normalization: true,
        temporal_randomization: true,
        phase_randomization: strength == CleanStrength::Aggressive,
        micro_timing_perturbation: true,
        human_imperfections: matches!(
            strength,
            CleanStrength::Preserving | CleanStrength::Aggressive
        ),
    }
}

/// Offline reference clean (same DSP chain as [`RealtimeProcessor`]).
pub fn offline_clean(samples: &[f32], sr: u32, strength: CleanStrength) -> Vec<f32> {
    let mut buffer = AudioBuffer::from_mono(samples.to_vec(), sr);
    let original_rms = buffer.rms();
    let config = defaults::default_config();
    let flags = config.advanced_flags;
    let fp = fp_config(strength);
    let mode = strength_to_mode(strength);
    let paranoid = strength == CleanStrength::Aggressive;
    let freq_ranges: &[(f64, f64)] = &[];

    match mode {
        SanitizationMode::Fast => {}
        SanitizationMode::Standard => {
            SpectralCleaner::clean(&mut buffer, paranoid, &flags, freq_ranges).unwrap();
            FingerprintRemover::remove(&mut buffer, paranoid, &fp).unwrap();
        }
        SanitizationMode::Preserving => {
            SpectralCleaner::clean(&mut buffer, paranoid, &flags, freq_ranges).unwrap();
            FingerprintRemover::remove(&mut buffer, paranoid, &fp).unwrap();
            StealthOps::apply(&mut buffer, &flags, paranoid).unwrap();
        }
        SanitizationMode::Aggressive => {
            SpectralCleaner::clean(&mut buffer, true, &flags, freq_ranges).unwrap();
            FingerprintRemover::remove(&mut buffer, true, &fp).unwrap();
            StealthOps::apply(&mut buffer, &flags, true).unwrap();
        }
    }

    if buffer.rms() > 1e-10 && original_rms > 1e-10 {
        buffer.normalize_rms(original_rms);
    }
    buffer.soft_clip(0.99);
    buffer.to_mono_samples()
}

pub fn render_blocks(
    rt: &mut RealtimeProcessor,
    input: &[f32],
    block_size: usize,
    flush_silence_blocks: usize,
) -> Vec<f32> {
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < input.len() {
        let end = (offset + block_size).min(input.len());
        let mut block = input[offset..end].to_vec();
        let mut channels: Vec<&mut [f32]> = vec![&mut block];
        rt.process_block(&mut channels);
        output.extend_from_slice(&block);
        offset = end;
    }
    for _ in 0..flush_silence_blocks {
        let mut block = vec![0.0f32; block_size];
        let mut channels: Vec<&mut [f32]> = vec![&mut block];
        rt.process_block(&mut channels);
        output.extend_from_slice(&block);
    }
    output
}

pub fn render_clean_aligned(
    rt: &mut RealtimeProcessor,
    input: &[f32],
    block_size: usize,
) -> Vec<f32> {
    let flush_blocks = (CLEAN_WINDOW_SAMPLES / block_size).max(1) + 4;
    let raw = render_blocks(rt, input, block_size, flush_blocks);
    if raw.len() <= CLEAN_WINDOW_SAMPLES {
        return vec![0.0; input.len()];
    }
    raw[CLEAN_WINDOW_SAMPLES
        ..CLEAN_WINDOW_SAMPLES + input.len().min(raw.len() - CLEAN_WINDOW_SAMPLES)]
        .to_vec()
}

pub fn rt_clean(samples: &[f32], sr: u32, strength: CleanStrength) -> Vec<f32> {
    let mut rt = RealtimeProcessor::new(sr, 1);
    rt.reset(sr, 1, BLOCK_SIZE);
    rt.set_strength(strength);
    rt.set_mode(OperationMode::Clean);
    render_clean_aligned(&mut rt, samples, BLOCK_SIZE)
}

pub fn rt_bypass(samples: &[f32], sr: u32) -> Vec<f32> {
    let mut rt = RealtimeProcessor::new(sr, 1);
    rt.reset(sr, 1, BLOCK_SIZE);
    rt.set_mode(OperationMode::Bypass);
    render_blocks(&mut rt, samples, BLOCK_SIZE, 0)
}

pub fn rt_detect(samples: &[f32], sr: u32) -> Vec<f32> {
    let mut rt = RealtimeProcessor::new(sr, 1);
    rt.reset(sr, 1, BLOCK_SIZE);
    rt.set_mode(OperationMode::Detect);
    let out = render_blocks(&mut rt, samples, BLOCK_SIZE, 0);
    rt.flush_detect_worker();
    out
}

pub fn mean_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).abs())
        .sum::<f32>()
        / n as f32
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}
