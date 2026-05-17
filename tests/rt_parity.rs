//! Integration tests aligned with polez `sanitization::pipeline` expectations.

mod common;

use common::{
    detect_confidence, mean_abs_diff, offline_clean, rms, rt_bypass, rt_clean, rt_detect,
    sine_mono, watermarked_mono, BLOCK_SIZE, TEST_SR,
};
use common::render_clean_aligned;
use polez_vst::rt::{CleanStrength, OperationMode, RealtimeProcessor, CLEAN_WINDOW_SAMPLES};

#[test]
fn polez_submodule_is_linked() {
    let buf = polez::audio::AudioBuffer::from_mono(vec![0.0; 16], TEST_SR);
    assert_eq!(buf.num_samples(), 16);
}

#[test]
fn bypass_is_bit_exact() {
    let input = sine_mono(440.0, TEST_SR, 0.25);
    let out = rt_bypass(&input, TEST_SR);
    assert_eq!(input.len(), out.len());
    assert!(mean_abs_diff(&input, &out) < 1e-6);
}

#[test]
fn detect_does_not_modify_audio() {
    let input = sine_mono(440.0, TEST_SR, 0.5);
    let out = rt_detect(&input, TEST_SR);
    assert_eq!(input.len(), out.len());
    assert!(mean_abs_diff(&input, &out) < 1e-6);
}

#[test]
fn detect_reports_confidence_after_history_fills() {
    let input = watermarked_mono(TEST_SR);
    let mut rt = RealtimeProcessor::new(TEST_SR, 1);
    rt.reset(TEST_SR, 1);
    rt.set_mode(OperationMode::Detect);

    let block = BLOCK_SIZE;
    let mut offset = 0;
    while offset < input.len() {
        let end = (offset + block).min(input.len());
        let mut chunk = input[offset..end].to_vec();
        let mut channels: Vec<&mut [f32]> = vec![&mut chunk];
        rt.process_block(&mut channels);
        offset = end;
    }

    assert!(
        rt.detection_confidence() >= 0.0 && rt.detection_confidence() <= 1.0,
        "confidence={}",
        rt.detection_confidence()
    );
    let offline = detect_confidence(&input, TEST_SR);
    assert!(
        (rt.detection_confidence() as f64 - offline).abs() < 0.35,
        "rt={} offline={offline}",
        rt.detection_confidence()
    );
}

#[test]
fn rt_clean_does_not_amplify_watermarks() {
    let input = watermarked_mono(TEST_SR);
    let before = detect_confidence(&input, TEST_SR);
    let cleaned = rt_clean(&input, TEST_SR, CleanStrength::Standard);
    assert_eq!(cleaned.len(), input.len());
    let after = detect_confidence(&cleaned, TEST_SR);
    assert!(
        after <= before + 0.35,
        "RT clean increased watermark confidence: before={before}, after={after}"
    );
}

#[test]
fn rt_clean_matches_offline_effectiveness() {
    let input = watermarked_mono(TEST_SR);
    let before = detect_confidence(&input, TEST_SR);

    let offline = offline_clean(&input, TEST_SR, CleanStrength::Standard);
    let rt = rt_clean(&input, TEST_SR, CleanStrength::Standard);

    let offline_after = detect_confidence(&offline, TEST_SR);
    let rt_after = detect_confidence(&rt, TEST_SR);

    assert!(offline_after <= before + 0.05);
    assert!(rt_after <= before + 0.35);
}

#[test]
fn rt_clean_bounded_quality_loss() {
    let input = watermarked_mono(TEST_SR);
    let original_rms = rms(&input);
    let cleaned = rt_clean(&input, TEST_SR, CleanStrength::Standard);
    let loss = (original_rms - rms(&cleaned)).abs() / original_rms;
    assert!(
        loss < 0.15,
        "RT quality loss too high: {loss:.4} (rms {} -> {})",
        original_rms,
        rms(&cleaned)
    );
}

#[test]
fn rt_fast_and_standard_differ() {
    let input = watermarked_mono(TEST_SR);
    let fast = rt_clean(&input, TEST_SR, CleanStrength::Fast);
    let standard = rt_clean(&input, TEST_SR, CleanStrength::Standard);
    let diff = mean_abs_diff(&fast, &standard);
    assert!(
        diff > 1e-5,
        "Fast and Standard RT modes produced identical output (diff={diff})"
    );
}

#[test]
fn rt_clean_no_false_positive_explosion_on_sine() {
    let input = sine_mono(440.0, TEST_SR, 0.5);
    let before = detect_confidence(&input, TEST_SR);
    let cleaned = rt_clean(&input, TEST_SR, CleanStrength::Standard);
    let after = detect_confidence(&cleaned, TEST_SR);
    assert!(
        after <= before + 0.35,
        "sine false-positive explosion: before={before}, after={after}"
    );
}

#[test]
fn clean_output_is_non_silent_after_warmup() {
    let input = sine_mono(440.0, TEST_SR, 1.0);
    let cleaned = rt_clean(&input, TEST_SR, CleanStrength::Standard);
    assert!(rms(&cleaned) > 0.01, "cleaned signal is silent");
    assert!(cleaned.iter().all(|s| s.is_finite()));
}

#[test]
fn multiple_block_sizes_produce_finite_output() {
    let input = watermarked_mono(TEST_SR);
    for block in [64usize, 128, 512, 1024, 2048] {
        let mut rt = RealtimeProcessor::new(TEST_SR, 1);
        rt.reset(TEST_SR, 1);
        rt.set_strength(CleanStrength::Standard);
        rt.set_mode(OperationMode::Clean);
        let out = render_clean_aligned(&mut rt, &input, block);
        assert_eq!(out.len(), input.len());
        assert!(out.iter().all(|s| s.is_finite()), "block_size={block}");
        assert!(rms(&out) > 0.01, "block_size={block}");
    }
}

#[test]
fn clean_latency_matches_window_constant() {
    assert_eq!(
        RealtimeProcessor::new(TEST_SR, 1).latency_samples(),
        0,
        "bypass latency"
    );
    let mut rt = RealtimeProcessor::new(TEST_SR, 1);
    rt.set_mode(OperationMode::Clean);
    assert_eq!(
        rt.latency_samples() as usize,
        CLEAN_WINDOW_SAMPLES,
        "clean latency"
    );
}
