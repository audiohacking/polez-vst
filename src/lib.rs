//! Polez real-time effect — VST3/CLAP plugin wrapping the polez forensics engine.

pub mod rt;

pub use rt::{CLEAN_WINDOW_SAMPLES, CleanStrength, OperationMode, RealtimeProcessor};
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

pub struct PolezVst {
    params: Arc<PolezVstParams>,
    rt: RealtimeProcessor,
}

impl PolezVst {
    pub fn new(params: Arc<PolezVstParams>) -> Self {
        Self {
            params,
            rt: RealtimeProcessor::new(48_000, 2),
        }
    }

    fn sync_params(&mut self) {
        let mode: Mode = self.params.mode.value();
        let strength: Strength = self.params.strength.value();
        self.rt.set_mode(mode.into());
        self.rt.set_strength(strength.into());
        self.rt.set_paranoid(self.params.paranoid.value());
    }
}

impl PluginLogic for PolezVst {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        self.rt.reset(sample_rate as u32, 2);
        self.sync_params();
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
        self.sync_params();

        let num_samples = buffer.num_samples();
        let channels = buffer.channels();
        if num_samples == 0 || channels == 0 {
            return ProcessStatus::Normal;
        }

        let mut channel_bufs: Vec<Vec<f32>> = Vec::with_capacity(channels);
        for ch in 0..channels {
            let (inp, out) = buffer.io(ch);
            let samples = inp.to_vec();
            channel_bufs.push(samples);
            if !std::ptr::eq(inp.as_ptr(), out.as_ptr()) {
                out.copy_from_slice(inp);
            }
        }

        let mut channel_slices: Vec<&mut [f32]> =
            channel_bufs.iter_mut().map(|v| v.as_mut_slice()).collect();
        self.rt.process_block(&mut channel_slices);

        for (ch, buf) in channel_bufs.iter().enumerate().take(channels) {
            let (_, out) = buffer.io(ch);
            out.copy_from_slice(buf);
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
}
