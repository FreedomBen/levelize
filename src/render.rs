//! Pass-2 rendering: the lookahead true-peak limiter.
//!
//! Topology, per frame, gain linked across channels, all math in f64:
//!
//!   1. detector: 4×-oversampled peak estimate (16-tap polyphase interpolator)
//!   2. required gain `ceiling / peak`, with a small margin for detector error
//!   3. release: one-pole recovery toward unity, instant snap down
//!   4. sliding minimum over the next `lookahead` frames (monotonic deque)
//!   5. boxcar average of that minimum, `lookahead` long, giving linear ramps
//!
//! Min-then-boxcar makes the cap hard: every value averaged into the gain at
//! frame `p` comes from a window that contains `p`, so each is already at or
//! below `p`'s required gain, and so is their mean. The signal is delayed by
//! detector lead + lookahead internally; output frame `n` is input frame `n`.

use std::collections::VecDeque;

use anyhow::{Result, ensure};

/// Safety margin under the ceiling covering detector interpolation error (dB).
const MARGIN_DB: f64 = 0.3;
/// Lookahead, which is also the gain ramp length (seconds).
const LOOKAHEAD_SECS: f64 = 0.005;
/// Gain recovery time constant after a reduction (seconds).
const RELEASE_SECS: f64 = 0.2;
/// Detector interpolator: 4× oversampling, 16 taps per phase.
const PHASES: usize = 4;
const TAPS: usize = 16;
/// The interpolator's center sits this many frames behind the newest sample.
const DETECTOR_LEAD: usize = 7;

pub struct Limiter {
    channels: usize,
    /// Linear ceiling, margin included.
    ceiling: f64,
    /// Per-frame one-pole release coefficient.
    release: f64,
    /// Lookahead in frames.
    lookahead: u64,
    taps: [[f64; TAPS]; PHASES - 1],
    /// Last TAPS samples per channel, ring-indexed by frame number.
    history: Vec<[f64; TAPS]>,
    /// Input frames waiting for their gain (interleaved).
    delay: VecDeque<f64>,
    frames_in: u64,
    real_frames: u64,
    floor_prev: f64,
    /// Sliding-minimum window over the release-smoothed floor: (index, value).
    minwin: VecDeque<(u64, f64)>,
    /// Boxcar state over the min curve.
    mins: VecDeque<f64>,
    min_sum: f64,
    emitted: u64,
    out: Vec<f32>,
}

impl Limiter {
    pub fn new(sample_rate: u32, channels: u16, ceiling_db: f64) -> Self {
        let lookahead = ((LOOKAHEAD_SECS * f64::from(sample_rate)).round() as u64).max(1);
        Limiter {
            channels: usize::from(channels),
            ceiling: 10f64.powf((ceiling_db - MARGIN_DB) / 20.0),
            release: (-1.0 / (RELEASE_SECS * f64::from(sample_rate))).exp(),
            lookahead,
            taps: detector_taps(),
            history: vec![[0.0; TAPS]; usize::from(channels)],
            delay: VecDeque::new(),
            frames_in: 0,
            real_frames: 0,
            floor_prev: 1.0,
            minwin: VecDeque::new(),
            mins: VecDeque::new(),
            min_sum: 0.0,
            emitted: 0,
            out: Vec::new(),
        }
    }

    /// Feed interleaved frames; `sink` receives the limited frames that became
    /// ready (the pipeline's internal delay means output trails input).
    pub fn process<F>(&mut self, interleaved: &[f32], mut sink: F) -> Result<()>
    where
        F: FnMut(&[f32]) -> Result<()>,
    {
        self.out.clear();
        for frame in interleaved.chunks_exact(self.channels) {
            self.real_frames += 1;
            self.step(frame);
        }
        if !self.out.is_empty() {
            sink(&self.out)?;
        }
        Ok(())
    }

    /// Drain the delay line; call once after the last `process`.
    pub fn flush<F>(mut self, mut sink: F) -> Result<()>
    where
        F: FnMut(&[f32]) -> Result<()>,
    {
        self.out.clear();
        let silence = vec![0.0f32; self.channels];
        let max_pad = self.lookahead + DETECTOR_LEAD as u64 + 2;
        for _ in 0..max_pad {
            if self.emitted >= self.real_frames {
                break;
            }
            self.step(&silence);
        }
        ensure!(
            self.emitted == self.real_frames,
            "limiter drained {} of {} frames (internal bug)",
            self.emitted,
            self.real_frames
        );
        if !self.out.is_empty() {
            sink(&self.out)?;
        }
        Ok(())
    }

    fn step(&mut self, frame: &[f32]) {
        let index = self.frames_in;
        self.frames_in += 1;
        let slot = (index % TAPS as u64) as usize;
        for (channel, sample) in frame.iter().enumerate() {
            self.history[channel][slot] = f64::from(*sample);
            self.delay.push_back(f64::from(*sample));
        }
        if index < DETECTOR_LEAD as u64 {
            return;
        }
        let center = index - DETECTOR_LEAD as u64;

        let peak = self.detect(index);
        let required = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        let floor = required.min(1.0 + (self.floor_prev - 1.0) * self.release);
        self.floor_prev = floor;

        while self.minwin.back().is_some_and(|&(_, value)| value >= floor) {
            self.minwin.pop_back();
        }
        self.minwin.push_back((center, floor));
        if center + 1 < self.lookahead {
            return;
        }
        let window_start = center + 1 - self.lookahead;
        while self
            .minwin
            .front()
            .is_some_and(|&(index, _)| index < window_start)
        {
            self.minwin.pop_front();
        }
        let window_min = self.minwin.front().map_or(1.0, |&(_, value)| value);

        self.mins.push_back(window_min);
        self.min_sum += window_min;
        if self.mins.len() as u64 > self.lookahead {
            self.min_sum -= self.mins.pop_front().unwrap_or(0.0);
        }
        let gain = self.min_sum / self.mins.len() as f64;
        self.emit(gain);
    }

    fn emit(&mut self, gain: f64) {
        for _ in 0..self.channels {
            let sample = self.delay.pop_front().unwrap_or(0.0);
            self.out.push((sample * gain) as f32);
        }
        self.emitted += 1;
    }

    /// Peak estimate for the frame `DETECTOR_LEAD` behind `index`: the sample
    /// itself plus the three interpolated points after it, per channel.
    fn detect(&self, index: u64) -> f64 {
        let center_slot = ((index - DETECTOR_LEAD as u64) % TAPS as u64) as usize;
        let mut peak = 0.0f64;
        for history in &self.history {
            peak = peak.max(history[center_slot].abs());
            for taps in &self.taps {
                let mut acc = 0.0;
                for (tap_index, tap) in taps.iter().enumerate() {
                    let slot = ((index + TAPS as u64 - tap_index as u64) % TAPS as u64) as usize;
                    acc += history[slot] * tap;
                }
                peak = peak.max(acc.abs());
            }
        }
        peak
    }
}

/// Windowed-sinc interpolation taps for the three fractional phases.
/// Tap `t` multiplies `x[i − t]`, whose offset from the interpolation center
/// (`DETECTOR_LEAD` behind the newest sample) is `t − DETECTOR_LEAD`.
fn detector_taps() -> [[f64; TAPS]; PHASES - 1] {
    std::array::from_fn(|phase_index| {
        let fraction = (phase_index + 1) as f64 / PHASES as f64;
        let mut taps: [f64; TAPS] = std::array::from_fn(|tap_index| {
            let distance = (tap_index as f64 - DETECTOR_LEAD as f64) + fraction;
            sinc(distance) * hann(distance / 9.0)
        });
        let sum: f64 = taps.iter().sum();
        for tap in &mut taps {
            *tap /= sum;
        }
        taps
    })
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

fn hann(u: f64) -> f64 {
    if u.abs() >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (std::f64::consts::PI * u).cos())
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;
    use crate::meter::{Loudness, Meter};

    const RATE: u32 = 48_000;

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

    fn run_limiter(channels: u16, ceiling_db: f64, input: &[f32]) -> Vec<f32> {
        let mut limiter = Limiter::new(RATE, channels, ceiling_db);
        let mut output = Vec::with_capacity(input.len());
        for chunk in input.chunks(1024 * usize::from(channels)) {
            limiter
                .process(chunk, |frames| {
                    output.extend_from_slice(frames);
                    Ok(())
                })
                .unwrap();
        }
        limiter
            .flush(|frames| {
                output.extend_from_slice(frames);
                Ok(())
            })
            .unwrap();
        output
    }

    fn measure(channels: u16, samples: &[f32]) -> Loudness {
        let mut meter = Meter::new(RATE, channels).unwrap();
        meter.feed(samples).unwrap();
        meter.finish().unwrap()
    }

    #[test]
    fn signals_below_the_ceiling_pass_bit_exact() {
        let input = sine_with_phase(2, 997.0, -6.0, 0.5, 0.0);
        let output = run_limiter(2, -1.5, &input);
        assert_eq!(input, output);
    }

    #[test]
    fn intersample_peaks_are_held_under_the_ceiling() {
        // fs/4 at 45° phase: samples sit 3 dB below the true peak (0 dBTP).
        let input = sine_with_phase(2, f64::from(RATE) / 4.0, 0.0, 1.0, PI / 4.0);
        let output = run_limiter(2, -1.5, &input);
        let loudness = measure(2, &output);
        assert!(
            loudness.true_peak_dbtp <= -1.5,
            "{}",
            loudness.true_peak_dbtp
        );
        // ... without being crushed far below it.
        assert!(
            loudness.true_peak_dbtp >= -2.5,
            "{}",
            loudness.true_peak_dbtp
        );
    }

    #[test]
    fn gain_recovers_after_a_burst() {
        let mut input = sine_with_phase(2, 997.0, -30.0, 1.0, 0.0);
        input.extend(sine_with_phase(2, 997.0, 0.0, 0.05, 0.0));
        input.extend(sine_with_phase(2, 997.0, -30.0, 1.0, 0.0));
        let output = run_limiter(2, -1.5, &input);
        assert_eq!(input.len(), output.len());
        let loudness = measure(2, &output);
        assert!(
            loudness.true_peak_dbtp <= -1.5,
            "{}",
            loudness.true_peak_dbtp
        );
        // A second after the burst, the release has returned the gain to ~1.
        let tail = input.len() - 2 * 4800;
        for (sample_in, sample_out) in input[tail..].iter().zip(&output[tail..]) {
            assert!((sample_in - sample_out).abs() < 2e-3);
        }
    }

    #[test]
    fn short_inputs_drain_completely() {
        let input = vec![0.25f32; 2 * 10];
        let output = run_limiter(2, -1.5, &input);
        assert_eq!(input, output);
    }
}
