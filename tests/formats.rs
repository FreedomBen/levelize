//! M4 acceptance: FLAC, MP3, and M4B/M4A output, compressed/AIFF input,
//! tag copying.

mod common;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::Command;

use common::{noise, sine, write_wav_float32, write_wav_pcm16};
use levelize::decode::{Decoder, Tags};
use levelize::encode::{Chapter, WavSpec, WavWriter};
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

fn have_ffmpeg() -> bool {
    levelize::encode::check_ffmpeg().is_ok()
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn m4b_output_lands_near_target_with_tags_and_chapters() {
    if !have_ffmpeg() {
        eprintln!("skipping: `ffmpeg` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.m4b");
    write_tagged_wav(&input);

    let settings = Settings {
        format: Format::M4b,
        chapters: vec![
            Chapter {
                start_secs: 0.0,
                title: "Intro".into(),
            },
            Chapter {
                start_secs: 4.0,
                title: "Part Two".into(),
            },
        ],
        ..Settings::default()
    };
    let report = levelize::level(&input, Output::File(&output), &settings).unwrap();
    assert!(report.gain_db > 0.0);
    assert_eq!(report.chapters_written, 2);

    // Lossy coding and codec delay nudge the numbers; stay within 1 LU.
    let out = levelize::analyze(&output).unwrap();
    assert!(
        (out.loudness.integrated_lufs + 16.0).abs() <= 1.0,
        "{}",
        out.loudness.integrated_lufs
    );
    assert!(
        (out.duration_secs - 8.0).abs() < 0.1,
        "{}",
        out.duration_secs
    );

    // Tags come back through symphonia's MP4 reader...
    let decoder = Decoder::open(&output).unwrap();
    let tags = decoder.tags().clone();
    assert_eq!(tags.title, Some("Episode 42".into()));
    assert_eq!(tags.artist, Some("levelize tests".into()));
    assert_eq!(tags.album, Some("Acceptance".into()));

    // ...and the audiobook marker and both chapter titles are in the atoms.
    let bytes = std::fs::read(&output).unwrap();
    assert!(contains(&bytes, b"stik"), "no audiobook media-type atom");
    assert!(contains(&bytes, b"chpl"), "no chapter list atom");
    assert!(contains(&bytes, b"Intro"));
    assert!(contains(&bytes, b"Part Two"));
}

#[test]
fn m4a_output_is_deterministic_and_not_an_audiobook() {
    if !have_ffmpeg() {
        eprintln!("skipping: `ffmpeg` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    write_wav_pcm16(&input, 44_100, 1, &sine(44_100, 1, 997.0, -30.0, 5.0));

    let out_a = dir.path().join("a.m4a");
    let out_b = dir.path().join("b.m4a");
    levelize::level(&input, Output::File(&out_a), &settings(Format::M4a)).unwrap();
    levelize::level(&input, Output::File(&out_b), &settings(Format::M4a)).unwrap();
    let bytes = std::fs::read(&out_a).unwrap();
    assert_eq!(bytes, std::fs::read(&out_b).unwrap());

    assert_eq!(&bytes[4..8], b"ftyp");
    assert_eq!(&bytes[8..12], b"M4A ");
    assert!(
        !contains(&bytes, b"stik"),
        "a plain .m4a must not claim to be an audiobook"
    );
}

#[test]
fn lossy_output_bitrate_follows_a_lossy_input() {
    if !have_ffmpeg() || levelize::encode::check_lame().is_err() {
        eprintln!("skipping: needs both `ffmpeg` and `lame`");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    write_wav_pcm16(&input, 44_100, 2, &noise(44_100, 2, -20.0, 10.0));

    // A 192 kbps MP3 input...
    let mp3 = dir.path().join("in.mp3");
    let settings_192 = Settings {
        format: Format::Mp3,
        mp3_bitrate_kbps: Some(192),
        ..Settings::default()
    };
    levelize::level(&input, Output::File(&mp3), &settings_192).unwrap();

    let kbps = |path: &Path| std::fs::metadata(path).unwrap().len() as f64 * 8.0 / 10.0 / 1000.0;

    // ...levels to M4B at ~192 kbps by default, not the static 96...
    let matched = dir.path().join("matched.m4b");
    levelize::level(&mp3, Output::File(&matched), &settings(Format::M4b)).unwrap();
    let matched_kbps = kbps(&matched);
    assert!(
        (150.0..240.0).contains(&matched_kbps),
        "expected ~192 kbps from a 192 kbps input, got {matched_kbps:.0}"
    );

    // ...while a lossless input keeps the static default (96 stereo).
    let fallback = dir.path().join("default.m4b");
    levelize::level(&input, Output::File(&fallback), &settings(Format::M4b)).unwrap();
    let fallback_kbps = kbps(&fallback);
    assert!(
        fallback_kbps < 130.0,
        "expected ~96 kbps from a WAV input, got {fallback_kbps:.0}"
    );

    // MP3 output matches too, snapped to a real CBR step.
    let remp3 = dir.path().join("matched.mp3");
    levelize::level(&mp3, Output::File(&remp3), &settings(Format::Mp3)).unwrap();
    let remp3_kbps = kbps(&remp3);
    assert!(
        (170.0..215.0).contains(&remp3_kbps),
        "expected 192 kbps CBR from a 192 kbps input, got {remp3_kbps:.0}"
    );
}

#[test]
fn m4b_streams_to_stdout_with_chapters() {
    if !have_ffmpeg() {
        eprintln!("skipping: `ffmpeg` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    write_wav_pcm16(&input, 44_100, 1, &sine(44_100, 1, 997.0, -30.0, 4.0));
    let toc = dir.path().join("toc.txt");
    std::fs::write(&toc, "0 Intro\n0:02 Outro\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_levelize"))
        .args(["--format", "m4b", "--chapters"])
        .arg(&toc)
        .args(["-o", "-"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = output.stdout;
    assert_eq!(
        &bytes[4..8],
        b"ftyp",
        "stdout does not start with an MP4 box"
    );
    assert!(contains(&bytes, b"chpl"));
    assert!(contains(&bytes, b"Outro"));
}

#[test]
fn input_chapters_carry_over_unless_overridden_or_stripped() {
    if !have_ffmpeg() {
        eprintln!("skipping: `ffmpeg` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("in.wav");
    write_wav_pcm16(&wav, 44_100, 1, &sine(44_100, 1, 997.0, -30.0, 6.0));

    // A chaptered source audiobook.
    let source = dir.path().join("src.m4b");
    let chaptered = Settings {
        format: Format::M4b,
        chapters: vec![
            Chapter {
                start_secs: 0.0,
                title: "Opening".into(),
            },
            Chapter {
                start_secs: 3.0,
                title: "Closing".into(),
            },
        ],
        ..Settings::default()
    };
    levelize::level(&wav, Output::File(&source), &chaptered).unwrap();

    // Re-leveling it carries the marks over by default...
    let kept = dir.path().join("kept.m4b");
    let report = levelize::level(&source, Output::File(&kept), &settings(Format::M4b)).unwrap();
    assert_eq!(report.chapters_written, 2, "{:?}", report.chapter_warning);
    let bytes = std::fs::read(&kept).unwrap();
    assert!(contains(&bytes, b"chpl"));
    assert!(contains(&bytes, b"Opening"));
    assert!(contains(&bytes, b"Closing"));

    // ...explicit chapters replace them (M4A takes the same path)...
    let replaced = dir.path().join("replaced.m4a");
    let fresh = Settings {
        format: Format::M4a,
        chapters: vec![Chapter {
            start_secs: 0.0,
            title: "Fresh".into(),
        }],
        ..Settings::default()
    };
    let report = levelize::level(&source, Output::File(&replaced), &fresh).unwrap();
    assert_eq!(report.chapters_written, 1);
    let bytes = std::fs::read(&replaced).unwrap();
    assert!(contains(&bytes, b"Fresh"));
    assert!(!contains(&bytes, b"Opening"));

    // ...keep_chapters = false (--no-chapters) strips them...
    let stripped = dir.path().join("stripped.m4b");
    let none = Settings {
        format: Format::M4b,
        keep_chapters: false,
        ..Settings::default()
    };
    let report = levelize::level(&source, Output::File(&stripped), &none).unwrap();
    assert_eq!(report.chapters_written, 0);
    let bytes = std::fs::read(&stripped).unwrap();
    assert!(!contains(&bytes, b"chpl"));
    assert!(!contains(&bytes, b"Opening"));

    // ...and a chaptered input still levels to chapterless formats cleanly.
    let plain = dir.path().join("out.wav");
    let report = levelize::level(&source, Output::File(&plain), &settings(Format::Wav)).unwrap();
    assert_eq!(report.chapters_written, 0);
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
