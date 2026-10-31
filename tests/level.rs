//! Acceptance tests for the leveling pipeline (DESIGN.md "Testing").

mod common;

use std::f64::consts::PI;
use std::path::Path;

use common::{sine, sine_with_phase, write_wav_pcm16};
use levelize::{Format, Output, Settings};

fn level_file(input: &Path, output: &Path, settings: &Settings) -> levelize::LevelReport {
    levelize::level(input, Output::File(output), settings).unwrap()
}

/// Decode a finished output back into memory for section-level checks.
fn read_samples(path: &Path) -> (u32, u16, Vec<f32>) {
    let decoder = levelize::decode::Decoder::open(path).unwrap();
    let info = *decoder.info();
    let mut samples = Vec::new();
    decoder
        .stream(|chunk| {
            samples.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    (info.sample_rate, info.channels, samples)
}

#[test]
fn levels_stereo_to_the_podcast_target() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 10.0));

    let report = level_file(&input, &output, &Settings::default());
    assert!((report.gain_db - 14.0).abs() < 0.2, "{}", report.gain_db);
    assert!(!report.gain_clamped);

    let out = levelize::analyze(&output).unwrap();
    assert_eq!(out.sample_rate, 48_000);
    assert_eq!(out.channels, 2);
    assert!(
        (out.duration_secs - 10.0).abs() < 0.01,
        "{}",
        out.duration_secs
    );
    assert!(
        (out.loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        out.loudness.integrated_lufs
    );
    assert!(
        out.loudness.true_peak_dbtp <= -1.5,
        "{}",
        out.loudness.true_peak_dbtp
    );
}

#[test]
fn mono_gets_the_mono_target() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    write_wav_pcm16(&input, 44_100, 1, &sine(44_100, 1, 997.0, -30.0, 10.0));

    level_file(&input, &output, &Settings::default());
    let out = levelize::analyze(&output).unwrap();
    assert!(
        (out.loudness.integrated_lufs + 19.0).abs() <= 0.5,
        "{}",
        out.loudness.integrated_lufs
    );
}

#[test]
fn ceiling_holds_against_intersample_transients() {
    // Quiet program with a full-scale fs/4 burst at 45° phase: its true peak
    // (0 dBTP) sits 3 dB above its samples, and the static boost would push it
    // far above the ceiling without the limiter.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    let mut samples = sine(48_000, 2, 997.0, -30.0, 4.0);
    samples.extend(sine_with_phase(48_000, 2, 12_000.0, 0.0, 0.01, PI / 4.0));
    samples.extend(sine(48_000, 2, 997.0, -30.0, 4.0));
    write_wav_pcm16(&input, 48_000, 2, &samples);

    let report = level_file(&input, &output, &Settings::default());
    assert!(report.gain_db > 0.0, "{}", report.gain_db);

    let out = levelize::analyze(&output).unwrap();
    assert!(
        out.loudness.true_peak_dbtp <= -1.5,
        "{}",
        out.loudness.true_peak_dbtp
    );
}

#[test]
fn boost_is_capped_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -45.0, 8.0));

    let report = level_file(&input, &output, &Settings::default());
    assert!(report.gain_clamped);
    assert!((report.gain_db - 15.0).abs() < 1e-9, "{}", report.gain_db);

    let out = levelize::analyze(&output).unwrap();
    assert!(
        (out.loudness.integrated_lufs + 30.0).abs() <= 0.5,
        "{}",
        out.loudness.integrated_lufs
    );
}

#[test]
fn levels_two_speakers_to_one_loudness() {
    // The Levelator case: 15 s at −30, a 1 s silence, 15 s at −15. A static
    // gain can't narrow the 15 dB spread; the adaptive curve must land both
    // speakers on the target, with the gain jump hidden in the pause.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    let mut samples = sine(48_000, 2, 997.0, -30.0, 15.0);
    samples.extend(std::iter::repeat_n(0.0f32, 2 * 48_000));
    samples.extend(sine(48_000, 2, 997.0, -15.0, 15.0));
    write_wav_pcm16(&input, 48_000, 2, &samples);

    let report = level_file(&input, &output, &Settings::default());
    assert!(
        report.input.loudness.lra_lu > 8.0,
        "fixture should have a wide range, got {}",
        report.input.loudness.lra_lu
    );
    assert!(!report.gain_clamped);
    assert!(
        report.gain_max_db - report.gain_min_db > 5.0,
        "curve should move between speakers: {} … {}",
        report.gain_min_db,
        report.gain_max_db
    );

    let out = levelize::analyze(&output).unwrap();
    assert!(
        (out.loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        out.loudness.integrated_lufs
    );
    assert!(
        out.loudness.lra_lu <= 2.0,
        "both speakers should sit at one loudness, LRA {}",
        out.loudness.lra_lu
    );
    assert!(
        out.loudness.true_peak_dbtp <= -1.5,
        "{}",
        out.loudness.true_peak_dbtp
    );
}

#[test]
fn pauses_hold_gain_instead_of_pumping_room_tone() {
    // Speech at −25 around 5 s of −40 "room tone": the tone must get the
    // held speech gain (≈ +9 dB, landing near −31 LUFS), not be tracked
    // toward the target (+15 dB after the clamp, landing near −25).
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    let mut samples = sine(48_000, 2, 997.0, -25.0, 10.0);
    samples.extend(sine(48_000, 2, 997.0, -40.0, 5.0));
    samples.extend(sine(48_000, 2, 997.0, -25.0, 10.0));
    write_wav_pcm16(&input, 48_000, 2, &samples);

    level_file(&input, &output, &Settings::default());
    let out = levelize::analyze(&output).unwrap();
    assert!(
        (out.loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        out.loudness.integrated_lufs
    );

    // Meter just the middle of the tone section (11.5 s … 13.5 s).
    let (rate, channels, samples) = read_samples(&output);
    assert_eq!(rate, 48_000);
    let start = 2 * ((11.5 * 48_000.0) as usize);
    let end = 2 * ((13.5 * 48_000.0) as usize);
    let mut meter = levelize::meter::Meter::new(rate, channels).unwrap();
    meter.feed(&samples[start..end]).unwrap();
    let tone = meter.finish().unwrap();
    assert!(
        tone.integrated_lufs <= -28.5,
        "room tone was boosted toward the target: {} LUFS",
        tone.integrated_lufs
    );
    assert!(
        tone.integrated_lufs >= -36.0,
        "room tone lost the held gain: {} LUFS",
        tone.integrated_lufs
    );
}

#[test]
fn output_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 5.0));

    let out_a = dir.path().join("a.wav");
    let out_b = dir.path().join("b.wav");
    level_file(&input, &out_a, &Settings::default());
    level_file(&input, &out_b, &Settings::default());
    assert_eq!(
        std::fs::read(&out_a).unwrap(),
        std::fs::read(&out_b).unwrap()
    );
}

#[test]
fn silence_passes_through_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    write_wav_pcm16(&input, 48_000, 2, &vec![0.0; 2 * 48_000 * 2]);

    let report = level_file(&input, &output, &Settings::default());
    assert!(report.silent_input);
    assert_eq!(report.gain_db, 0.0);

    // 16-bit TPDF dither is the only signal left: at or below ~-86 dBTP.
    let out = levelize::analyze(&output).unwrap();
    assert!(
        out.loudness.integrated_lufs < -70.0,
        "{}",
        out.loudness.integrated_lufs
    );
    assert!(
        out.loudness.true_peak_dbtp < -80.0,
        "{}",
        out.loudness.true_peak_dbtp
    );
}

#[test]
fn leveling_is_idempotent_within_tolerance() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let once = dir.path().join("once.wav");
    let twice = dir.path().join("twice.wav");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 10.0));

    level_file(&input, &once, &Settings::default());
    let report = level_file(&once, &twice, &Settings::default());
    assert!(report.gain_db.abs() <= 0.3, "{}", report.gain_db);

    let out = levelize::analyze(&twice).unwrap();
    assert!(
        (out.loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        out.loudness.integrated_lufs
    );
}

#[test]
fn refuses_non_wav_formats_for_now() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.flac");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 1.0));

    let settings = Settings {
        format: Format::Flac,
        ..Settings::default()
    };
    let error = levelize::level(&input, Output::File(&output), &settings).unwrap_err();
    assert!(error.to_string().contains("M4"), "{error}");
}
