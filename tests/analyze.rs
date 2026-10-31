//! End-to-end `analyze` tests over generated WAV files.

use std::f64::consts::PI;
use std::path::Path;

/// Minimal 16-bit PCM WAV writer for fixtures (levelize's own writer lands in M2).
fn write_wav_pcm16(path: &Path, rate: u32, channels: u16, samples: &[f32]) {
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
fn sine(rate: u32, channels: u16, freq: f64, amplitude_dbfs: f64, seconds: f64) -> Vec<f32> {
    let amplitude = 10f64.powf(amplitude_dbfs / 20.0);
    let frames = (seconds * f64::from(rate)) as usize;
    (0..frames)
        .flat_map(|frame| {
            let t = frame as f64 / f64::from(rate);
            let sample = (amplitude * (2.0 * PI * freq * t).sin()) as f32;
            std::iter::repeat_n(sample, usize::from(channels))
        })
        .collect()
}

#[test]
fn analyze_reports_stereo_wav_stats() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav_pcm16(&path, 48_000, 2, &sine(48_000, 2, 997.0, -23.0, 10.0));

    let report = levelize::analyze(&path).unwrap();
    assert_eq!(report.sample_rate, 48_000);
    assert_eq!(report.channels, 2);
    assert!(
        (report.duration_secs - 10.0).abs() < 0.01,
        "{}",
        report.duration_secs
    );
    assert!(
        (report.loudness.integrated_lufs + 23.0).abs() <= 0.15,
        "{}",
        report.loudness.integrated_lufs
    );
    assert!(
        (report.loudness.true_peak_dbtp + 23.0).abs() <= 0.3,
        "{}",
        report.loudness.true_peak_dbtp
    );
}

#[test]
fn analyze_reports_mono_wav_stats() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mono.wav");
    write_wav_pcm16(&path, 44_100, 1, &sine(44_100, 1, 997.0, -23.0, 10.0));

    let report = levelize::analyze(&path).unwrap();
    assert_eq!(report.sample_rate, 44_100);
    assert_eq!(report.channels, 1);
    assert!(
        (report.loudness.integrated_lufs + 26.0).abs() <= 0.15,
        "{}",
        report.loudness.integrated_lufs
    );
}

#[test]
fn analyze_rejects_three_channel_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("surround.wav");
    write_wav_pcm16(&path, 48_000, 3, &sine(48_000, 3, 997.0, -23.0, 1.0));

    let error = levelize::analyze(&path).unwrap_err().to_string();
    assert!(error.contains("mono and stereo"), "{error}");
}

#[test]
fn analyze_reports_missing_file() {
    assert!(levelize::analyze(Path::new("does-not-exist.wav")).is_err());
}
