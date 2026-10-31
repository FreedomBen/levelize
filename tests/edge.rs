//! Hardening tests: pathological inputs either level cleanly or fail cleanly.

mod common;

use std::path::{Path, PathBuf};

use common::{sine, write_wav_float32, write_wav_pcm16};
use levelize::{Output, Settings, analyze, level};

fn level_default(input: &Path) -> (levelize::LevelReport, PathBuf) {
    let output = input.with_extension("leveled.wav");
    let report = level(input, Output::File(&output), &Settings::default()).unwrap();
    (report, output)
}

#[test]
fn empty_input_copies_through_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("empty.wav");
    write_wav_pcm16(&input, 48_000, 2, &[]);

    let (report, output) = level_default(&input);
    assert!(report.silent_input);
    assert_eq!(report.gain_db, 0.0);
    assert!(report.output_loudness.integrated_lufs.is_infinite());

    let out = analyze(&output).unwrap();
    assert_eq!(out.duration_secs, 0.0);
}

#[test]
fn sub_hop_input_copies_through() {
    // 50 ms: shorter than one planner hop, far under the gating window.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("tiny.wav");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -20.0, 0.05));

    let (report, output) = level_default(&input);
    assert!(report.silent_input);

    let out = analyze(&output).unwrap();
    assert!(
        (out.duration_secs - 0.05).abs() < 1e-6,
        "{}",
        out.duration_secs
    );
    // Copied through: the peak must not have moved.
    assert!(
        (out.loudness.true_peak_dbtp - report.input.loudness.true_peak_dbtp).abs() < 0.1,
        "{} vs {}",
        out.loudness.true_peak_dbtp,
        report.input.loudness.true_peak_dbtp
    );
}

#[test]
fn truncated_file_levels_what_is_there() {
    // The header declares 6 s but the file holds 60% of that; the declared
    // length must only ever drive progress display, never the output.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("trunc.wav");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 440.0, -30.0, 6.0));
    let bytes = std::fs::read(&input).unwrap();
    let keep = 44 + (bytes.len() - 44) * 6 / 10 / 4 * 4;
    std::fs::write(&input, &bytes[..keep]).unwrap();

    let (report, output) = level_default(&input);
    assert!(
        (report.input.duration_secs - 3.6).abs() < 0.05,
        "{}",
        report.input.duration_secs
    );
    assert!(
        (report.output_loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        report.output_loudness.integrated_lufs
    );

    let out = analyze(&output).unwrap();
    assert!((out.duration_secs - report.input.duration_secs).abs() < 1e-6);
}

#[test]
fn non_finite_samples_are_silenced() {
    // A float WAV with NaN and ±inf dropped into a quiet sine. Untreated,
    // one NaN poisons the loudness filters for the rest of the file and the
    // infinities slam the limiter; all three must come out as silence.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("nan.wav");
    let mut samples = sine(48_000, 2, 440.0, -26.0, 6.0);
    let mid = samples.len() / 2;
    samples[mid] = f32::NAN;
    samples[mid + 1] = f32::NAN;
    samples[mid + 100] = f32::INFINITY;
    samples[mid + 201] = f32::NEG_INFINITY;
    write_wav_float32(&input, 48_000, 2, &samples);

    let (report, output) = level_default(&input);
    assert!(report.input.loudness.integrated_lufs.is_finite());
    assert!(
        report.input.loudness.true_peak_dbtp < 0.0,
        "{}",
        report.input.loudness.true_peak_dbtp
    );
    assert!(
        (report.output_loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        report.output_loudness.integrated_lufs
    );

    // Float in → float out; every output sample must be finite. The data
    // chunk sits at the fixed tail of our tagless float-WAV header.
    let bytes = std::fs::read(&output).unwrap();
    assert_eq!(&bytes[50..54], b"data");
    let finite = bytes[58..]
        .chunks_exact(4)
        .all(|b| f32::from_le_bytes(b.try_into().unwrap()).is_finite());
    assert!(finite, "non-finite samples leaked into the output");
}

#[test]
fn more_than_two_channels_is_a_clean_error() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("quad.wav");
    write_wav_pcm16(&input, 48_000, 4, &sine(48_000, 4, 440.0, -20.0, 1.0));

    let error = analyze(&input).err().map(|e| e.to_string()).unwrap();
    assert!(error.contains("mono and stereo"), "{error}");
}

#[test]
fn non_audio_input_is_a_clean_error() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("garbage.wav");
    std::fs::write(&input, b"This is not audio at all, just text pretending.\n").unwrap();

    let error = analyze(&input).err().map(|e| e.to_string()).unwrap();
    assert!(error.contains("unrecognized or unsupported"), "{error}");
}

#[test]
fn extreme_sample_rates_level_on_target() {
    let dir = tempfile::tempdir().unwrap();
    for rate in [8_000u32, 192_000] {
        let input = dir.path().join(format!("rate{rate}.wav"));
        write_wav_pcm16(&input, rate, 2, &sine(rate, 2, 440.0, -30.0, 5.0));
        let (report, _) = level_default(&input);
        assert!(
            (report.output_loudness.integrated_lufs + 16.0).abs() <= 0.5,
            "{rate} Hz: {}",
            report.output_loudness.integrated_lufs
        );
        assert!(
            report.output_loudness.true_peak_dbtp <= -1.5,
            "{rate} Hz: {}",
            report.output_loudness.true_peak_dbtp
        );
    }
}
