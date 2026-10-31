//! Core engine for levelize — adaptive loudness leveling for spoken-word audio.
//!
//! See DESIGN.md: two streaming passes over the input. Pass 1 measures
//! BS.1770 loudness and plans a gain curve; pass 2 applies it, limits true
//! peaks, and encodes. The engine modules (decode, meter, plan, render,
//! encode) land with milestones M1–M4; this crate root holds the shared
//! configuration surface.

pub mod decode;
pub mod encode;
pub mod meter;
pub mod plan;
pub mod render;
pub mod stats;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use clap::ValueEnum;

pub use stats::{AnalyzeReport, LevelReport};

/// Loudness preset: supplies the default target and ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Profile {
    /// −16 LUFS stereo / −19 mono, −1.5 dBTP (Apple Podcasts)
    Podcast,
    /// −23 LUFS, −1 dBTP (EBU R128)
    Broadcast,
    /// −20 LUFS, −3 dBTP (lands inside ACX's RMS/peak bounds)
    Audiobook,
}

impl Profile {
    /// Default integrated-loudness target in LUFS for a channel count.
    pub fn target_lufs(self, channels: u16) -> f64 {
        match self {
            Profile::Podcast => {
                if channels == 1 {
                    -19.0
                } else {
                    -16.0
                }
            }
            Profile::Broadcast => -23.0,
            Profile::Audiobook => -20.0,
        }
    }

    /// Default true-peak ceiling in dBTP.
    pub fn ceiling_dbtp(self) -> f64 {
        match self {
            Profile::Podcast => -1.5,
            Profile::Broadcast => -1.0,
            Profile::Audiobook => -3.0,
        }
    }
}

/// How hard the gain curve converges on the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Leveling {
    Gentle,
    Normal,
    Aggressive,
}

/// Output container/codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Wav,
    Flac,
    /// Requires `lame` in PATH at run time.
    Mp3,
}

/// Output sample format for WAV/FLAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BitDepth {
    #[value(name = "16")]
    Int16,
    #[value(name = "24")]
    Int24,
    /// 32-bit float (WAV only)
    #[value(name = "32f")]
    Float32,
}

/// Full configuration for one leveling run.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub profile: Profile,
    /// Explicit target in LUFS; overrides the profile when set.
    pub target_lufs: Option<f64>,
    /// Explicit ceiling in dBTP; overrides the profile when set.
    pub ceiling_dbtp: Option<f64>,
    /// Cap on upward gain in dB.
    pub max_boost_db: f64,
    /// Cap on downward gain in dB.
    pub max_cut_db: f64,
    pub leveling: Leveling,
    pub format: Format,
    /// `None` matches the input's bit depth.
    pub bit_depth: Option<BitDepth>,
    /// CBR bitrate for MP3 output; `None` picks 128 kbps stereo / 96 mono.
    pub mp3_bitrate_kbps: Option<u32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            profile: Profile::Podcast,
            target_lufs: None,
            ceiling_dbtp: None,
            max_boost_db: 15.0,
            max_cut_db: 15.0,
            leveling: Leveling::Normal,
            format: Format::Wav,
            bit_depth: None,
            mp3_bitrate_kbps: None,
        }
    }
}

impl Settings {
    /// Effective loudness target for an input with `channels` channels.
    pub fn target_for(&self, channels: u16) -> f64 {
        self.target_lufs
            .unwrap_or_else(|| self.profile.target_lufs(channels))
    }

    /// Effective true-peak ceiling.
    pub fn ceiling(&self) -> f64 {
        self.ceiling_dbtp
            .unwrap_or_else(|| self.profile.ceiling_dbtp())
    }

    /// Effective MP3 bitrate in kbps for an input with `channels` channels.
    pub fn mp3_bitrate_for(&self, channels: u16) -> u32 {
        self.mp3_bitrate_kbps
            .unwrap_or(if channels == 1 { 96 } else { 128 })
    }
}

/// Decode one input end to end and meter it.
fn measure(path: &Path) -> Result<(decode::InputInfo, u64, meter::Loudness)> {
    let decoder = decode::Decoder::open(path)?;
    let info = *decoder.info();
    let mut meter = meter::Meter::new(info.sample_rate, info.channels)?;
    let frames = decoder.stream(|chunk| meter.feed(chunk))?;
    Ok((info, frames, meter.finish()?))
}

/// Pass-1 measurement of one input: decode and meter, write nothing.
pub fn analyze(path: &Path) -> Result<AnalyzeReport> {
    let (info, frames, loudness) = measure(path)?;
    Ok(AnalyzeReport {
        path: path.to_owned(),
        duration_secs: frames as f64 / f64::from(info.sample_rate),
        sample_rate: info.sample_rate,
        channels: info.channels,
        loudness,
    })
}

/// Where a leveling run writes its audio.
#[derive(Debug, Clone, Copy)]
pub enum Output<'a> {
    File(&'a Path),
    Stdout,
}

/// One pass-2 encoder, dispatched by output format. The FLAC writer is boxed
/// for its block buffers' sake; the variants would differ wildly in size.
enum Writer {
    Wav(encode::WavWriter<Box<dyn Write + Send>>),
    Flac(Box<encode::FlacWriter<Box<dyn Write + Send>>>),
    Mp3(encode::Mp3Writer),
}

impl Writer {
    fn write_frames(&mut self, interleaved: &[f32]) -> Result<()> {
        match self {
            Writer::Wav(writer) => writer.write_frames(interleaved),
            Writer::Flac(writer) => writer.write_frames(interleaved),
            Writer::Mp3(writer) => writer.write_frames(interleaved),
        }
    }

    fn finish(self) -> Result<Option<encode::FlacStats>> {
        match self {
            Writer::Wav(writer) => writer.finish().map(|()| None),
            Writer::Flac(writer) => writer.finish().map(Some),
            Writer::Mp3(writer) => writer.finish().map(|()| None),
        }
    }
}

/// Output bit depth for the format: the explicit setting, else the source
/// depth, else 16-bit — with float mapped into FLAC's integer range.
fn resolve_bit_depth(settings: &Settings, source: Option<BitDepth>) -> Result<BitDepth> {
    if settings.format == Format::Mp3 {
        ensure!(
            settings.bit_depth.is_none(),
            "MP3 output has no bit depth; drop --bit-depth"
        );
        return Ok(BitDepth::Int16); // fed to the WAV pipe's spec only
    }
    let depth = settings.bit_depth.or(source).unwrap_or(BitDepth::Int16);
    if settings.format == Format::Flac && depth == BitDepth::Float32 {
        ensure!(
            settings.bit_depth.is_none(),
            "FLAC output supports bit depths 16 and 24"
        );
        return Ok(BitDepth::Int24); // nearest FLAC can hold a float source
    }
    Ok(depth)
}

/// Level one input: measure and plan the gain curve (pass 1), then
/// re-decode, apply the curve, limit true peaks, and encode (pass 2).
pub fn level(input: &Path, output: Output<'_>, settings: &Settings) -> Result<LevelReport> {
    if settings.format == Format::Mp3 {
        encode::check_lame()?;
    }

    let decoder = decode::Decoder::open(input)?;
    let info = *decoder.info();
    let tags = decoder.tags().clone();
    let mut analyzer = plan::Analyzer::new(info.sample_rate, info.channels)?;
    decoder.stream(|chunk| analyzer.feed(chunk))?;
    let analysis = analyzer.finish()?;
    let frames = analysis.frames;
    let input_loudness = analysis.loudness;

    let target_lufs = settings.target_for(info.channels);
    let curve = plan::plan(&analysis, settings, target_lufs);
    let mut sampler = curve.sampler();

    let bit_depth = resolve_bit_depth(settings, info.source_depth)?;
    let (sink, output_name): (Box<dyn Write + Send>, String) = match output {
        Output::File(path) => (
            Box::new(BufWriter::new(
                File::create(path).with_context(|| format!("cannot create {}", path.display()))?,
            )),
            path.display().to_string(),
        ),
        Output::Stdout => (Box::new(BufWriter::new(std::io::stdout())), "-".to_string()),
    };
    let spec = encode::WavSpec {
        sample_rate: info.sample_rate,
        channels: info.channels,
        bit_depth,
    };
    let mut writer = match settings.format {
        Format::Wav => Writer::Wav(encode::WavWriter::new(sink, spec, frames, &tags)?),
        Format::Flac => Writer::Flac(Box::new(encode::FlacWriter::new(
            sink, spec, frames, &tags,
        )?)),
        Format::Mp3 => Writer::Mp3(encode::Mp3Writer::new(
            sink,
            info.sample_rate,
            info.channels,
            frames,
            settings.mp3_bitrate_for(info.channels),
            &tags,
        )?),
    };
    let mut out_meter = meter::Meter::new(info.sample_rate, info.channels)?;
    let mut limiter = render::Limiter::new(info.sample_rate, info.channels, settings.ceiling());

    let decoder = decode::Decoder::open(input)?;
    let channels = usize::from(info.channels);
    let mut gained: Vec<f32> = Vec::new();
    decoder.stream(|chunk| {
        gained.clear();
        for frame in chunk.chunks_exact(channels) {
            let gain = sampler.next_gain();
            gained.extend(frame.iter().map(|s| (f64::from(*s) * gain) as f32));
        }
        limiter.process(&gained, |limited| {
            out_meter.feed(limited)?;
            writer.write_frames(limited)
        })
    })?;
    limiter.flush(|limited| {
        out_meter.feed(limited)?;
        writer.write_frames(limited)
    })?;
    let flac_stats = writer.finish()?;
    if let (Some(stats), Output::File(path)) = (flac_stats, output) {
        encode::patch_flac_header(path, &stats)?;
    }

    Ok(LevelReport {
        input: AnalyzeReport {
            path: input.to_owned(),
            duration_secs: frames as f64 / f64::from(info.sample_rate),
            sample_rate: info.sample_rate,
            channels: info.channels,
            loudness: input_loudness,
        },
        output_name,
        output_loudness: out_meter.finish()?,
        gain_db: curve.median_db,
        gain_min_db: curve.min_db,
        gain_max_db: curve.max_db,
        gain_clamped: curve.clamped,
        silent_input: curve.all_pause,
        target_lufs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn podcast_target_depends_on_channel_count() {
        assert_eq!(Profile::Podcast.target_lufs(2), -16.0);
        assert_eq!(Profile::Podcast.target_lufs(1), -19.0);
    }

    #[test]
    fn broadcast_and_audiobook_targets_ignore_channel_count() {
        assert_eq!(Profile::Broadcast.target_lufs(1), -23.0);
        assert_eq!(Profile::Broadcast.target_lufs(2), -23.0);
        assert_eq!(Profile::Audiobook.target_lufs(1), -20.0);
        assert_eq!(Profile::Audiobook.target_lufs(2), -20.0);
    }

    #[test]
    fn profile_ceilings_match_the_design_doc() {
        assert_eq!(Profile::Podcast.ceiling_dbtp(), -1.5);
        assert_eq!(Profile::Broadcast.ceiling_dbtp(), -1.0);
        assert_eq!(Profile::Audiobook.ceiling_dbtp(), -3.0);
    }

    #[test]
    fn explicit_values_override_the_profile() {
        let settings = Settings {
            target_lufs: Some(-14.0),
            ceiling_dbtp: Some(-2.0),
            ..Settings::default()
        };
        assert_eq!(settings.target_for(1), -14.0);
        assert_eq!(settings.ceiling(), -2.0);
    }

    #[test]
    fn profile_supplies_defaults_when_not_overridden() {
        let settings = Settings {
            profile: Profile::Broadcast,
            ..Settings::default()
        };
        assert_eq!(settings.target_for(2), -23.0);
        assert_eq!(settings.ceiling(), -1.0);
    }

    #[test]
    fn mp3_bitrate_defaults_follow_channel_count() {
        let settings = Settings::default();
        assert_eq!(settings.mp3_bitrate_for(1), 96);
        assert_eq!(settings.mp3_bitrate_for(2), 128);

        let settings = Settings {
            mp3_bitrate_kbps: Some(64),
            ..Settings::default()
        };
        assert_eq!(settings.mp3_bitrate_for(2), 64);
    }
}
