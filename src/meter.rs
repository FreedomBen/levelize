//! BS.1770 / EBU R128 loudness measurement over streamed f32 chunks.

use anyhow::{Result, anyhow};
use ebur128::{EbuR128, Mode};

/// Measured loudness of one whole input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// Integrated (gated) loudness in LUFS; `-inf` for all-silent input.
    pub integrated_lufs: f64,
    /// Loudness range in LU.
    pub lra_lu: f64,
    /// Maximum true peak across channels in dBTP; `-inf` for digital silence.
    pub true_peak_dbtp: f64,
}

/// Streaming meter: feed interleaved f32 chunks, then `finish`.
pub struct Meter {
    state: EbuR128,
    channels: u16,
}

impl Meter {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self> {
        let state = EbuR128::new(
            u32::from(channels),
            sample_rate,
            Mode::I | Mode::LRA | Mode::TRUE_PEAK | Mode::M | Mode::S,
        )
        .map_err(meter_error)?;
        Ok(Meter { state, channels })
    }

    /// Feed one interleaved chunk of whole frames.
    pub fn feed(&mut self, interleaved: &[f32]) -> Result<()> {
        self.state.add_frames_f32(interleaved).map_err(meter_error)
    }

    /// Momentary loudness (last 400 ms) in LUFS; `-inf` over silence.
    /// The window is zero-padded until 400 ms have been fed.
    pub fn momentary(&self) -> Result<f64> {
        self.state.loudness_momentary().map_err(meter_error)
    }

    /// Short-term loudness (last 3 s) in LUFS; `-inf` over silence.
    /// The window is zero-padded until 3 s have been fed.
    pub fn short_term(&self) -> Result<f64> {
        self.state.loudness_shortterm().map_err(meter_error)
    }

    /// Final measurements over everything fed so far.
    pub fn finish(self) -> Result<Loudness> {
        let integrated_lufs = self.state.loudness_global().map_err(meter_error)?;
        let lra_lu = self.state.loudness_range().map_err(meter_error)?;
        let peak = (0..u32::from(self.channels))
            .map(|channel| self.state.true_peak(channel))
            .try_fold(0.0_f64, |max, peak| peak.map(|peak| max.max(peak)))
            .map_err(meter_error)?;
        Ok(Loudness {
            integrated_lufs,
            lra_lu,
            true_peak_dbtp: amplitude_to_dbtp(peak),
        })
    }
}

fn meter_error(error: ebur128::Error) -> anyhow::Error {
    anyhow!("loudness meter: {error}")
}

/// Linear amplitude → dBTP (`-inf` at 0).
fn amplitude_to_dbtp(amplitude: f64) -> f64 {
    20.0 * amplitude.log10()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    const RATE: u32 = 48_000;

    /// Interleaved sine, the same signal on every channel.
    fn sine(channels: u16, freq: f64, amplitude_dbfs: f64, seconds: f64) -> Vec<f32> {
        sine_with_phase(channels, freq, amplitude_dbfs, seconds, 0.0)
    }

    fn sine_with_phase(
        channels: u16,
        freq: f64,
        amplitude_dbfs: f64,
        seconds: f64,
        phase: f64,
    ) -> Vec<f32> {
        let amplitude = 10f64.powf(amplitude_dbfs / 20.0);
        let frames = (seconds * f64::from(RATE)) as usize;
        (0..frames)
            .flat_map(|frame| {
                let t = frame as f64 / f64::from(RATE);
                let sample = (amplitude * (2.0 * PI * freq * t + phase).sin()) as f32;
                std::iter::repeat_n(sample, usize::from(channels))
            })
            .collect()
    }

    fn measure(channels: u16, samples: &[f32]) -> Loudness {
        let mut meter = Meter::new(RATE, channels).unwrap();
        meter.feed(samples).unwrap();
        meter.finish().unwrap()
    }

    // EBU Tech 3341 case 1: a 997 Hz stereo sine at −23 dBFS reads −23.0 LUFS.
    #[test]
    fn tech3341_stereo_sine_at_minus_23() {
        let loudness = measure(2, &sine(2, 997.0, -23.0, 10.0));
        assert!(
            (loudness.integrated_lufs + 23.0).abs() <= 0.1,
            "{}",
            loudness.integrated_lufs
        );
    }

    // EBU Tech 3341 case 2: the same tone at −33 dBFS reads −33.0 LUFS.
    #[test]
    fn tech3341_stereo_sine_at_minus_33() {
        let loudness = measure(2, &sine(2, 997.0, -33.0, 10.0));
        assert!(
            (loudness.integrated_lufs + 33.0).abs() <= 0.1,
            "{}",
            loudness.integrated_lufs
        );
    }

    // BS.1770: a single-channel 997 Hz sine reads 3.01 LU below the stereo pair.
    #[test]
    fn mono_sine_reads_three_lu_below_stereo() {
        let loudness = measure(1, &sine(1, 997.0, -23.0, 10.0));
        assert!(
            (loudness.integrated_lufs + 26.0).abs() <= 0.1,
            "{}",
            loudness.integrated_lufs
        );
    }

    // EBU Tech 3341 case 3 shape: passages below the relative gate don't drag
    // the integrated value down.
    #[test]
    fn tech3341_gating_excludes_quiet_passages() {
        let mut samples = sine(2, 997.0, -36.0, 5.0);
        samples.extend(sine(2, 997.0, -23.0, 30.0));
        samples.extend(sine(2, 997.0, -36.0, 5.0));
        let loudness = measure(2, &samples);
        assert!(
            (loudness.integrated_lufs + 23.0).abs() <= 0.1,
            "{}",
            loudness.integrated_lufs
        );
    }

    // EBU Tech 3342 case 1: 20 s at −20 dBFS then 20 s at −30 dBFS → LRA 10 LU.
    #[test]
    fn tech3342_two_level_lra() {
        let mut samples = sine(2, 997.0, -20.0, 20.0);
        samples.extend(sine(2, 997.0, -30.0, 20.0));
        let loudness = measure(2, &samples);
        assert!((loudness.lra_lu - 10.0).abs() <= 1.0, "{}", loudness.lra_lu);
    }

    // A 997 Hz tone's true peak matches its amplitude.
    #[test]
    fn true_peak_matches_amplitude_for_slow_tones() {
        let loudness = measure(2, &sine(2, 997.0, -6.0, 2.0));
        assert!(
            (loudness.true_peak_dbtp + 6.0).abs() <= 0.2,
            "{}",
            loudness.true_peak_dbtp
        );
    }

    // Tech 3341 cases 21–23 shape: an fs/4 tone at 45° phase has every sample
    // 3 dB below its true peak; the meter must see between the samples.
    #[test]
    fn true_peak_catches_intersample_peaks() {
        let samples = sine_with_phase(2, f64::from(RATE) / 4.0, -6.02, 2.0, PI / 4.0);
        let loudness = measure(2, &samples);
        assert!(
            loudness.true_peak_dbtp > -6.6 && loudness.true_peak_dbtp < -5.8,
            "{}",
            loudness.true_peak_dbtp
        );
    }

    // Digital silence: everything gates out, peaks sit at −inf.
    #[test]
    fn silence_reads_negative_infinity() {
        let samples = vec![0.0f32; 2 * 48_000 * 5];
        let loudness = measure(2, &samples);
        assert!(loudness.integrated_lufs < -70.0);
        assert!(loudness.true_peak_dbtp.is_infinite() && loudness.true_peak_dbtp < 0.0);
    }
}
