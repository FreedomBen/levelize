//! End-to-end `analyze` tests over generated WAV files.

mod common;

use std::path::Path;

use common::{sine, write_wav_pcm16};

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
