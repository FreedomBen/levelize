//! Core engine for levelize — adaptive loudness leveling for spoken-word audio.
//!
//! See DESIGN.md: two streaming passes over the input. Pass 1 measures
//! BS.1770 loudness and plans a gain curve; pass 2 applies it, limits true
//! peaks, and encodes. The engine modules (decode, meter, plan, render,
//! encode) land with milestones M1–M4; this crate root holds the shared
//! configuration surface.

pub mod cli;
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
    /// AAC in an MP4 audiobook container; requires `ffmpeg` in PATH at run time.
    M4b,
    /// AAC in a plain MP4 audio container; requires `ffmpeg` in PATH at run time.
    M4a,
}

impl Format {
    /// The conventional file extension.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::Mp3 => "mp3",
            Format::M4b => "m4b",
            Format::M4a => "m4a",
        }
    }

    /// Display name for messages.
    pub fn label(self) -> &'static str {
        match self {
            Format::Wav => "WAV",
            Format::Flac => "FLAC",
            Format::Mp3 => "MP3",
            Format::M4b => "M4B",
            Format::M4a => "M4A",
        }
    }

    /// The AAC-in-MP4 outputs, rendered by ffmpeg.
    pub fn is_aac(self) -> bool {
        matches!(self, Format::M4b | Format::M4a)
    }
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
    /// CBR bitrate for MP3 output; `None` matches a lossy input's measured
    /// bitrate, else picks 128 kbps stereo / 96 mono.
    pub mp3_bitrate_kbps: Option<u32>,
    /// Bitrate for M4B/M4A (AAC) output; `None` matches a lossy input's
    /// measured bitrate, else picks 96 kbps stereo / 64 mono.
    pub aac_bitrate_kbps: Option<u32>,
    /// Chapter marks for M4B/M4A output.
    pub chapters: Vec<encode::Chapter>,
    /// When `chapters` is empty, carry chapter marks found in the input
    /// into M4B/M4A output.
    pub keep_chapters: bool,
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
            aac_bitrate_kbps: None,
            chapters: Vec::new(),
            keep_chapters: true,
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

    /// Effective MP3 bitrate in kbps: the explicit setting, else a lossy
    /// input's own bitrate snapped to MP3's CBR steps, else the default for
    /// the channel count.
    pub fn mp3_bitrate_for(&self, channels: u16, input_kbps: Option<u32>) -> u32 {
        self.mp3_bitrate_kbps
            .or(input_kbps.map(nearest_mp3_bitrate))
            .unwrap_or(if channels == 1 { 96 } else { 128 })
    }

    /// Effective AAC bitrate in kbps: the explicit setting, else a lossy
    /// input's own bitrate, else the default for the channel count.
    pub fn aac_bitrate_for(&self, channels: u16, input_kbps: Option<u32>) -> u32 {
        self.aac_bitrate_kbps
            .or(input_kbps)
            .unwrap_or(if channels == 1 { 64 } else { 96 })
    }
}

/// The nearest bitrate MP3 CBR can carry (MPEG-1 Layer III); ties snap down.
fn nearest_mp3_bitrate(kbps: u32) -> u32 {
    const STEPS: [u32; 14] = [
        32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    STEPS
        .into_iter()
        .min_by_key(|step| step.abs_diff(kbps))
        .expect("the table is not empty")
}

/// Decode one input end to end and meter it.
fn measure(path: &Path) -> Result<(decode::InputInfo, u64, meter::Loudness)> {
    let decoder = decode::Decoder::open(path)?;
    let info = *decoder.info();
    let mut meter = meter::Meter::new(info.sample_rate, info.channels)?;
    let frames = decoder.stream(|chunk| meter.feed(chunk))?.frames;
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
    Aac(encode::AacWriter),
}

impl Writer {
    fn write_frames(&mut self, interleaved: &[f32]) -> Result<()> {
        match self {
            Writer::Wav(writer) => writer.write_frames(interleaved),
            Writer::Flac(writer) => writer.write_frames(interleaved),
            Writer::Mp3(writer) => writer.write_frames(interleaved),
            Writer::Aac(writer) => writer.write_frames(interleaved),
        }
    }

    fn finish(self) -> Result<Option<encode::FlacStats>> {
        match self {
            Writer::Wav(writer) => writer.finish().map(|()| None),
            Writer::Flac(writer) => writer.finish().map(Some),
            Writer::Mp3(writer) => writer.finish().map(|()| None),
            Writer::Aac(writer) => writer.finish().map(|()| None),
        }
    }
}

/// Output bit depth for the format: the explicit setting, else the source
/// depth, else 16-bit — with float mapped into FLAC's integer range.
fn resolve_bit_depth(settings: &Settings, source: Option<BitDepth>) -> Result<BitDepth> {
    if settings.format == Format::Mp3 || settings.format.is_aac() {
        ensure!(
            settings.bit_depth.is_none(),
            "{} output has no bit depth; drop --bit-depth",
            settings.format.label()
        );
        return Ok(BitDepth::Int16); // a placeholder; the encoder pipes take floats
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

/// Where a [`level_with_progress`] callback currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Pass 1: measuring and planning.
    Analyze,
    /// Pass 2: rendering the output.
    Render,
}

/// Level one input: measure and plan the gain curve (pass 1), then
/// re-decode, apply the curve, limit true peaks, and encode (pass 2).
pub fn level(input: &Path, output: Output<'_>, settings: &Settings) -> Result<LevelReport> {
    level_with_progress(input, output, settings, |_, _, _| {})
}

/// [`level`], reporting progress as `(phase, seconds decoded, total seconds
/// if known)`. The total is exact in the render phase; in the analyze phase
/// it is whatever the container declares.
pub fn level_with_progress(
    input: &Path,
    output: Output<'_>,
    settings: &Settings,
    mut progress: impl FnMut(Phase, f64, Option<f64>),
) -> Result<LevelReport> {
    if settings.format == Format::Mp3 {
        encode::check_lame()?;
    }
    if settings.format.is_aac() {
        encode::check_ffmpeg()?;
    }
    ensure!(
        settings.chapters.is_empty() || settings.format.is_aac(),
        "chapters apply to M4B/M4A output only"
    );

    let decoder = decode::Decoder::open(input)?;
    let info = *decoder.info();
    let tags = decoder.tags().clone();
    let rate = f64::from(info.sample_rate);
    let declared_secs = info.total_frames.map(|frames| frames as f64 / rate);
    let mut analyzer = plan::Analyzer::new(info.sample_rate, info.channels)?;
    let mut decoded: u64 = 0;
    let streamed = decoder.stream(|chunk| {
        decoded += (chunk.len() / usize::from(info.channels)) as u64;
        progress(Phase::Analyze, decoded as f64 / rate, declared_secs);
        analyzer.feed(chunk)
    })?;
    let analysis = analyzer.finish()?;
    let frames = analysis.frames;
    let input_loudness = analysis.loudness;

    // A lossy input's measured bitrate is the default for lossy output.
    let input_kbps = (info.lossy && frames > 0).then(|| {
        let kbps = streamed.compressed_bytes as f64 * 8.0 * rate / (frames as f64 * 1000.0);
        (kbps.round() as u32).clamp(32, 320)
    });

    // Chapter marks: explicit settings win; otherwise an M4B/M4A render
    // carries over whatever the input itself declares. Chapter problems
    // never fail a render — the marks are auxiliary — they surface as a
    // report warning instead.
    let mut chapter_warning = None;
    let chapters: Vec<encode::Chapter> = if !settings.format.is_aac() {
        Vec::new()
    } else if !settings.chapters.is_empty() {
        settings.chapters.clone()
    } else if settings.keep_chapters {
        match encode::read_input_chapters(input) {
            Ok(found) => {
                let duration_secs = frames as f64 / rate;
                let total = found.len();
                let kept: Vec<encode::Chapter> = found
                    .into_iter()
                    .filter(|chapter| chapter.start_secs < duration_secs)
                    .collect();
                if kept.len() < total {
                    chapter_warning = Some(format!(
                        "{} input chapter mark(s) past the end of the audio were dropped",
                        total - kept.len()
                    ));
                }
                kept
            }
            Err(error) => {
                chapter_warning = Some(format!("input chapters not carried over: {error:#}"));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

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
            settings.mp3_bitrate_for(info.channels, input_kbps),
            &tags,
        )?),
        Format::M4b | Format::M4a => Writer::Aac(encode::AacWriter::new(
            sink,
            encode::AacSpec {
                sample_rate: info.sample_rate,
                channels: info.channels,
                total_frames: frames,
                bitrate_kbps: settings.aac_bitrate_for(info.channels, input_kbps),
                audiobook: settings.format == Format::M4b,
            },
            &tags,
            &chapters,
        )?),
    };
    let mut out_meter = meter::Meter::new(info.sample_rate, info.channels)?;
    let mut limiter = render::Limiter::new(info.sample_rate, info.channels, settings.ceiling());

    let decoder = decode::Decoder::open(input)?;
    let channels = usize::from(info.channels);
    let total_secs = frames as f64 / rate;
    let mut rendered: u64 = 0;
    let mut gained: Vec<f32> = Vec::new();
    decoder.stream(|chunk| {
        gained.clear();
        for frame in chunk.chunks_exact(channels) {
            let gain = sampler.next_gain();
            gained.extend(frame.iter().map(|s| (f64::from(*s) * gain) as f32));
        }
        rendered += (chunk.len() / channels) as u64;
        progress(Phase::Render, rendered as f64 / rate, Some(total_secs));
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
        chapters_written: chapters.len(),
        chapter_warning,
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
    fn lossy_bitrates_follow_flag_then_input_then_channel_count() {
        let settings = Settings::default();
        assert_eq!(settings.mp3_bitrate_for(1, None), 96);
        assert_eq!(settings.mp3_bitrate_for(2, None), 128);
        assert_eq!(settings.aac_bitrate_for(1, None), 64);
        assert_eq!(settings.aac_bitrate_for(2, None), 96);

        // A lossy input's own bitrate beats the static default — snapped to
        // a real CBR step for MP3 (ties snap down), taken as-is for AAC.
        assert_eq!(settings.aac_bitrate_for(2, Some(187)), 187);
        assert_eq!(settings.mp3_bitrate_for(2, Some(187)), 192);
        assert_eq!(settings.mp3_bitrate_for(2, Some(144)), 128);

        // The explicit flag beats both.
        let settings = Settings {
            mp3_bitrate_kbps: Some(64),
            aac_bitrate_kbps: Some(48),
            ..Settings::default()
        };
        assert_eq!(settings.mp3_bitrate_for(2, Some(187)), 64);
        assert_eq!(settings.aac_bitrate_for(2, Some(187)), 48);
    }
}
