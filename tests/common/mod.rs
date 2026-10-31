//! Shared fixture helpers for integration tests.
#![allow(dead_code)] // each test crate uses a subset

use std::f64::consts::PI;
use std::path::Path;

/// Minimal 16-bit PCM WAV writer for fixtures, independent of levelize's own
/// encoder so encode bugs can't hide in the fixtures.
pub fn write_wav_pcm16(path: &Path, rate: u32, channels: u16, samples: &[f32]) {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let quantized = (f64::from(*sample).clamp(-1.0, 1.0) * 32767.0).round() as i16;
        bytes.extend_from_slice(&quantized.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// Interleaved sine, the same signal on every channel.
pub fn sine(rate: u32, channels: u16, freq: f64, amplitude_dbfs: f64, seconds: f64) -> Vec<f32> {
    sine_with_phase(rate, channels, freq, amplitude_dbfs, seconds, 0.0)
}

pub fn sine_with_phase(
    rate: u32,
    channels: u16,
    freq: f64,
    amplitude_dbfs: f64,
    seconds: f64,
    phase: f64,
) -> Vec<f32> {
    let amplitude = 10f64.powf(amplitude_dbfs / 20.0);
    let frames = (seconds * f64::from(rate)) as usize;
    (0..frames)
        .flat_map(|frame| {
            let t = frame as f64 / f64::from(rate);
            let sample = (amplitude * (2.0 * PI * freq * t + phase).sin()) as f32;
            std::iter::repeat_n(sample, usize::from(channels))
        })
        .collect()
}
