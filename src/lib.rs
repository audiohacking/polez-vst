//! Polez real-time effect — VST3/CLAP plugin wrapping the polez forensics engine.

pub mod rt;

pub use rt::{
    CLEAN_WINDOW_SAMPLES, CleanStrength, DETECT_ANALYSIS_SAMPLES, OperationMode,
    POLEZ_MIN_DETECT_SAMPLES, RealtimeProcessor,
};
use std::sync::Arc;
use truce::prelude::*;
use truce_gui::layout::{GridLayout, dropdown, meter, toggle, widgets};

#[derive(ParamEnum)]
pub enum Mode {
    Bypass,
    Detect,
    Clean,
}

impl From<Mode> for OperationMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Bypass => OperationMode::Bypass,
            Mode::Detect => OperationMode::Detect,
            Mode::Clean => OperationMode::Clean,
        }
    }
}

#[derive(ParamEnum)]
pub enum Strength {
    Fast,
    Standard,
    Preserving,
    Aggressive,
}

impl From<Strength> for CleanStrength {
    fn from(s: Strength) -> Self {
        match s {
            Strength::Fast => CleanStrength::Fast,
            Strength::Standard => CleanStrength::Standard,
            Strength::Preserving => CleanStrength::Preserving,
            Strength::Aggressive => CleanStrength::Aggressive,
        }
    }
}

#[derive(Params)]
pub struct PolezVstParams {
    #[param(name = "Mode")]
    pub mode: EnumParam<Mode>,

    #[param(name = "Strength")]
    pub strength: EnumParam<Strength>,

    #[param(name = "Paranoid")]
    pub paranoid: BoolParam,

    #[meter]
    pub detect_meter: MeterSlot,
}

use PolezVstParamsParamId as P;

const MAX_HOST_CHANNELS: usize = 2;

pub struct PolezVst {
    params: Arc<PolezVstParams>,
    rt: RealtimeProcessor,
    scratch: [Vec<f32>; MAX_HOST_CHANNELS],
    cached_mode: Mode,
    cached_strength: Strength,
    cached_paranoid: bool,
}

impl PolezVst {
    pub fn new(params: Arc<PolezVstParams>) -> Self {
        let cached_mode = params.mode.value();
        let cached_strength = params.strength.value();
        let cached_paranoid = params.paranoid.value();
        let mut rt = RealtimeProcessor::new(48_000, MAX_HOST_CHANNELS);
        rt.set_mode(cached_mode.into());
        rt.set_strength(cached_strength.into());
        rt.set_paranoid(cached_paranoid);
        Self {
            params,
            rt,
            scratch: std::array::from_fn(|_| Vec::new()),
            cached_mode,
            cached_strength,
            cached_paranoid,
        }
    }

    fn sync_params_if_changed(&mut self) {
        let mode = self.params.mode.value();
        let strength = self.params.strength.value();
        let paranoid = self.params.paranoid.value();

        if mode != self.cached_mode {
            self.rt.set_mode(mode.into());
            self.cached_mode = mode;
        }
        if strength != self.cached_strength {
            self.rt.set_strength(strength.into());
            self.cached_strength = strength;
        }
        if paranoid != self.cached_paranoid {
            self.rt.set_paranoid(paranoid);
            self.cached_paranoid = paranoid;
        }
    }

    fn preallocate_scratch(&mut self, max_block_size: usize) {
        for ch in &mut self.scratch {
            if ch.len() < max_block_size {
                ch.resize(max_block_size, 0.0);
            }
        }
    }

    /// Copy input → output when the host uses separate buffers (Logic, pluginval, etc.).
    fn passthrough_buffer(&mut self, buffer: &mut AudioBuffer, channels: usize) {
        for ch in 0..channels {
            let (inp, out) = buffer.io(ch);
            if !std::ptr::eq(inp.as_ptr(), out.as_ptr()) {
                out.copy_from_slice(inp);
            }
        }
    }

    fn process_detect(&mut self, buffer: &mut AudioBuffer, num_samples: usize, channels: usize) {
        self.rt.poll_detect_results();
        self.passthrough_buffer(buffer, channels);
        match channels {
            1 => self.rt.ingest_detect_block(&[buffer.input(0)], num_samples),
            2 => self
                .rt
                .ingest_detect_block(&[buffer.input(0), buffer.input(1)], num_samples),
            _ => {
                self.preallocate_scratch(num_samples);
                let n = channels.min(MAX_HOST_CHANNELS);
                for ch in 0..n {
                    self.scratch[ch][..num_samples].copy_from_slice(buffer.input(ch));
                }
                let refs: Vec<&[f32]> = (0..n).map(|ch| &self.scratch[ch][..num_samples]).collect();
                self.rt.ingest_detect_block(&refs, num_samples);
            }
        }
    }

    fn process_clean(&mut self, buffer: &mut AudioBuffer, num_samples: usize, channels: usize) {
        self.preallocate_scratch(num_samples);

        for ch in 0..channels.min(MAX_HOST_CHANNELS) {
            self.scratch[ch][..num_samples].copy_from_slice(buffer.input(ch));
        }

        if channels == 1 {
            self.rt
                .process_block(&mut [&mut self.scratch[0][..num_samples]]);
        } else if channels == 2 {
            let (ch0, ch1) = self.scratch.split_at_mut(1);
            self.rt
                .process_block(&mut [&mut ch0[0][..num_samples], &mut ch1[0][..num_samples]]);
        } else {
            for ch in 0..channels.min(MAX_HOST_CHANNELS) {
                let slice = &mut self.scratch[ch][..num_samples];
                self.rt.process_block(&mut [slice]);
            }
        }

        for ch in 0..channels.min(MAX_HOST_CHANNELS) {
            buffer
                .output(ch)
                .copy_from_slice(&self.scratch[ch][..num_samples]);
        }
    }
}

impl PluginLogic for PolezVst {
    fn reset(&mut self, sample_rate: f64, max_block_size: usize) {
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        let block = max_block_size.max(1);
        self.rt.reset(sample_rate as u32, MAX_HOST_CHANNELS, block);
        self.sync_params_if_changed();
        self.preallocate_scratch(block);
    }

    fn latency(&self) -> u32 {
        self.rt.latency_samples()
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        context: &mut ProcessContext,
    ) -> ProcessStatus {
        self.sync_params_if_changed();

        let num_samples = buffer.num_samples();
        let channels = buffer.channels();
        if num_samples == 0 || channels == 0 {
            return ProcessStatus::Normal;
        }

        match self.rt.mode() {
            OperationMode::Bypass => self.passthrough_buffer(buffer, channels),
            OperationMode::Detect => {
                self.process_detect(buffer, num_samples, channels);
            }
            OperationMode::Clean => {
                self.process_clean(buffer, num_samples, channels);
            }
        }

        context.set_meter(P::DetectMeter, self.rt.detection_confidence());
        ProcessStatus::Normal
    }

    fn layout(&self) -> GridLayout {
        GridLayout::build(vec![widgets(vec![
            dropdown(P::Mode, "Mode"),
            dropdown(P::Strength, "Strength"),
            toggle(P::Paranoid, "Paranoid"),
            meter(&[P::DetectMeter], "Detection")
                .at(0, 2)
                .cols(2)
                .rows(2),
        ])])
        .with_title("POLEZ")
    }
}

truce::plugin! {
    logic: PolezVst,
    params: PolezVstParams,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_is_valid() {
        truce_test::assert_valid_info::<Plugin>();
    }

    #[test]
    fn driver_passthrough_bypass() {
        use std::time::Duration;
        use truce_test::{InputSource, assertions, driver};

        let result = driver!(Plugin)
            .duration(Duration::from_millis(50))
            .input(InputSource::Constant(0.25))
            .run();

        assertions::assert_no_nans(&result);
        assertions::assert_nonzero(&result);
    }

    #[test]
    fn driver_passthrough_detect() {
        use std::time::Duration;
        use truce_test::{InputSource, assertions, driver};

        let result = driver!(Plugin)
            .duration(Duration::from_millis(50))
            .input(InputSource::Constant(0.25))
            .script(|s| s.set_param(P::Mode, 1.0 / 2.0)) // Detect (Bypass=0, Detect=1, Clean=2)
            .run();

        assertions::assert_no_nans(&result);
        assertions::assert_nonzero(&result);
    }

    #[test]
    fn has_editor() {
        truce_test::assert_has_editor::<Plugin>();
    }

    #[test]
    fn state_round_trips() {
        truce_test::assert_state_round_trip::<Plugin>();
    }

    #[test]
    fn bus_config_effect() {
        truce_test::assert_bus_config_effect::<Plugin>();
    }

    #[test]
    fn param_count_matches() {
        truce_test::assert_param_count_matches::<Plugin>();
    }

    #[test]
    fn corrupt_state_no_crash() {
        truce_test::assert_corrupt_state_no_crash::<Plugin>();
    }
}
