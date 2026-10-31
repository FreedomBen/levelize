//! M4 acceptance: FLAC and MP3 output, compressed/AIFF input, tag copying.

mod common;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::Command;

use common::{sine, write_wav_float32, write_wav_pcm16};
use levelize::decode::{Decoder, Tags};
use levelize::encode::{WavSpec, WavWriter};
use levelize::{BitDepth, Format, Output, Settings};

fn settings(format: Format) -> Settings {
    Settings {
        format,
        ..Settings::default()
    }
}

fn demo_tags() -> Tags {
    Tags {
        title: Some("Episode 42".into()),
        artist: Some("levelize tests".into()),
        album: Some("Acceptance".into()),
        date: Some("2026".into()),
        genre: Some("Podcast".into()),
        comment: Some("round trip".into()),
        track: Some("42".into()),
    }
}

/// A −30 dBFS stereo WAV carrying the demo tags.
fn write_tagged_wav(path: &Path) {
    let samples = sine(48_000, 2, 997.0, -30.0, 8.0);
    let spec = WavSpec {
        sample_rate: 48_000,
        channels: 2,
        bit_depth: BitDepth::Int16,
    };
    let sink = BufWriter::new(File::create(path).unwrap());
    let mut writer = WavWriter::new(sink, spec, (samples.len() / 2) as u64, &demo_tags()).unwrap();
    writer.write_frames(&samples).unwrap();
    writer.finish().unwrap();
}

#[test]
fn flac_output_lands_on_target() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.flac");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 10.0));

    levelize::level(&input, Output::File(&output), &settings(Format::Flac)).unwrap();

    let report = levelize::analyze(&output).unwrap();
    assert_eq!(report.sample_rate, 48_000);
    assert_eq!(report.channels, 2);
    assert!(
        (report.duration_secs - 10.0).abs() < 0.01,
        "{}",
        report.duration_secs
    );
    assert!(
        (report.loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        report.loudness.integrated_lufs
    );
    assert!(
        report.loudness.true_peak_dbtp <= -1.5,
        "{}",
        report.loudness.true_peak_dbtp
    );
    // 16-bit input ⇒ 16-bit FLAC by default.
    let decoder = Decoder::open(&output).unwrap();
    assert_eq!(decoder.info().source_depth, Some(BitDepth::Int16));
}

#[test]
fn flac_output_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 5.0));

    let out_a = dir.path().join("a.flac");
    let out_b = dir.path().join("b.flac");
    levelize::level(&input, Output::File(&out_a), &settings(Format::Flac)).unwrap();
    levelize::level(&input, Output::File(&out_b), &settings(Format::Flac)).unwrap();
    assert_eq!(
        std::fs::read(&out_a).unwrap(),
        std::fs::read(&out_b).unwrap()
    );
}

#[test]
fn flac_output_satisfies_the_reference_decoder() {
    if Command::new("flac").arg("--version").output().is_err() {
        eprintln!("skipping: `flac` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.flac");
    write_wav_pcm16(&input, 44_100, 1, &sine(44_100, 1, 997.0, -30.0, 6.0));
    levelize::level(&input, Output::File(&output), &settings(Format::Flac)).unwrap();

    // -w turns warnings (e.g. a missing MD5) into errors; the patched header
    // must satisfy a full verification pass.
    let status = Command::new("flac")
        .args(["-t", "-s", "-w"])
        .arg(&output)
        .status()
        .unwrap();
    assert!(status.success(), "flac -t rejected the output: {status}");
}

#[test]
fn tags_survive_wav_to_flac_to_wav() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    write_tagged_wav(&input);

    // Our RIFF INFO chunk reads back through symphonia.
    let decoder = Decoder::open(&input).unwrap();
    assert_eq!(*decoder.tags(), demo_tags());

    // WAV → FLAC carries them as Vorbis comments...
    let flac = dir.path().join("out.flac");
    levelize::level(&input, Output::File(&flac), &settings(Format::Flac)).unwrap();
    let decoder = Decoder::open(&flac).unwrap();
    assert_eq!(*decoder.tags(), demo_tags());

    // ...and FLAC → WAV brings them back to RIFF INFO.
    let wav = dir.path().join("out.wav");
    levelize::level(&flac, Output::File(&wav), &settings(Format::Wav)).unwrap();
    let decoder = Decoder::open(&wav).unwrap();
    assert_eq!(*decoder.tags(), demo_tags());
}

#[test]
fn mp3_output_lands_near_target_with_tags() {
    if levelize::encode::check_lame().is_err() {
        eprintln!("skipping: `lame` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.mp3");
    write_tagged_wav(&input);

    let report = levelize::level(&input, Output::File(&output), &settings(Format::Mp3)).unwrap();
    assert!(report.gain_db > 0.0);

    // Lossy coding and codec delay nudge the numbers; stay within 1 LU.
    let out = levelize::analyze(&output).unwrap();
    assert!(
        (out.loudness.integrated_lufs + 16.0).abs() <= 1.0,
        "{}",
        out.loudness.integrated_lufs
    );

    let decoder = Decoder::open(&output).unwrap();
    let tags = decoder.tags().clone();
    assert_eq!(tags.title, Some("Episode 42".into()));
    assert_eq!(tags.artist, Some("levelize tests".into()));
    assert_eq!(tags.album, Some("Acceptance".into()));
    assert_eq!(tags.date, Some("2026".into()));
    assert_eq!(tags.genre, Some("Podcast".into()));
    assert_eq!(tags.comment, Some("round trip".into()));
    assert_eq!(tags.track, Some("42".into()));
}

#[test]
fn aiff_input_decodes_and_levels() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.aiff");
    let output = dir.path().join("out.wav");
    write_aiff_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -20.0, 4.0));

    let report = levelize::analyze(&input).unwrap();
    assert_eq!(report.sample_rate, 48_000);
    assert_eq!(report.channels, 2);
    assert!(
        (report.loudness.integrated_lufs + 20.0).abs() <= 0.3,
        "{}",
        report.loudness.integrated_lufs
    );

    levelize::level(&input, Output::File(&output), &settings(Format::Wav)).unwrap();
    let out = levelize::analyze(&output).unwrap();
    assert!(
        (out.loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        out.loudness.integrated_lufs
    );
}

#[test]
fn float_wav_input_stays_float_by_default() {
    // The WAV demuxer names float sources only by codec id; "match the
    // input's bit depth" must not quietly quantize them to 16-bit.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    write_wav_float32(&input, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 4.0));

    levelize::level(&input, Output::File(&output), &settings(Format::Wav)).unwrap();
    let bytes = std::fs::read(&output).unwrap();
    assert_eq!(&bytes[12..16], b"fmt ");
    let format_tag = u16::from_le_bytes(bytes[20..22].try_into().unwrap());
    let bits = u16::from_le_bytes(bytes[34..36].try_into().unwrap());
    assert_eq!(format_tag, 3, "IEEE float, not PCM");
    assert_eq!(bits, 32);
}

#[test]
fn float_bit_depth_is_rejected_for_flac() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.flac");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 1.0));

    let settings = Settings {
        format: Format::Flac,
        bit_depth: Some(BitDepth::Float32),
        ..Settings::default()
    };
    let error = levelize::level(&input, Output::File(&output), &settings).unwrap_err();
    assert!(error.to_string().contains("16 and 24"), "{error}");
}

/// Minimal big-endian PCM16 AIFF: FORM/AIFF with COMM and SSND chunks.
fn write_aiff_pcm16(path: &Path, sample_rate: u32, channels: u16, samples: &[f32]) {
    assert_eq!(sample_rate, 48_000, "fixture only knows 48 kHz");
    let frames = (samples.len() / usize::from(channels)) as u32;
    let data_len = samples.len() as u32 * 2;

    let mut comm = Vec::new();
    comm.extend_from_slice(&channels.to_be_bytes());
    comm.extend_from_slice(&frames.to_be_bytes());
    comm.extend_from_slice(&16u16.to_be_bytes());
    // 48000 as an 80-bit extended float.
    comm.extend_from_slice(&[0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]);

    let mut file = Vec::new();
    file.extend_from_slice(b"FORM");
    file.extend_from_slice(&(4 + 8 + 18 + 8 + 8 + data_len).to_be_bytes());
    file.extend_from_slice(b"AIFF");
    file.extend_from_slice(b"COMM");
    file.extend_from_slice(&18u32.to_be_bytes());
    file.extend_from_slice(&comm);
    file.extend_from_slice(b"SSND");
    file.extend_from_slice(&(8 + data_len).to_be_bytes());
    file.extend_from_slice(&0u32.to_be_bytes()); // offset
    file.extend_from_slice(&0u32.to_be_bytes()); // block size
    for sample in samples {
        let value = (f64::from(*sample) * 32767.0)
            .round()
            .clamp(-32768.0, 32767.0) as i16;
        file.extend_from_slice(&value.to_be_bytes());
    }
    File::create(path).unwrap().write_all(&file).unwrap();
}
