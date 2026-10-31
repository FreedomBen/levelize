//! Streaming decode of input audio via symphonia: WAV, AIFF, FLAC, MP3, AAC
//! (and .m4a), and Ogg Vorbis — plus the basic text tags they carry.

use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, bail};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{
    CODEC_TYPE_NULL, CODEC_TYPE_PCM_F32BE, CODEC_TYPE_PCM_F32LE, CODEC_TYPE_PCM_F64BE,
    CODEC_TYPE_PCM_F64LE, Decoder as SymphoniaDecoder, DecoderOptions,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
use symphonia::core::probe::Hint;
use symphonia::core::sample::SampleFormat;
use symphonia::core::units::Time;

use crate::BitDepth;

/// Basic text tags copied from input to output (DESIGN.md "Metadata").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub date: Option<String>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub track: Option<String>,
}

impl Tags {
    pub fn is_empty(&self) -> bool {
        *self == Tags::default()
    }
}

/// Decode packets until the first one yields audio, and report its signal
/// spec; used when the container doesn't declare the rate or channels.
fn peek_spec(
    format: &mut dyn FormatReader,
    decoder: &mut dyn SymphoniaDecoder,
    track_id: u32,
) -> Result<(u32, usize)> {
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(ref error)) if error.kind() == ErrorKind::UnexpectedEof => {
                bail!("no decodable audio frames")
            }
            Err(error) => return Err(error).context("reading input"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = decoded.spec();
                return Ok((spec.rate, spec.channels.count()));
            }
            // A frame the decoder can't make sense of yet; keep looking.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(error).context("probing input"),
        }
    }
}

/// Fold one metadata revision into `tags`; earlier finds win.
fn collect_tags(tags: &mut Tags, revision: &MetadataRevision) {
    for tag in revision.tags() {
        let Some(key) = tag.std_key else { continue };
        let slot = match key {
            StandardTagKey::TrackTitle => &mut tags.title,
            StandardTagKey::Artist => &mut tags.artist,
            StandardTagKey::Album => &mut tags.album,
            StandardTagKey::Date => &mut tags.date,
            StandardTagKey::Genre => &mut tags.genre,
            StandardTagKey::Comment => &mut tags.comment,
            StandardTagKey::TrackNumber => &mut tags.track,
            _ => continue,
        };
        if slot.is_none() {
            // RIFF INFO values come back NUL-terminated; strip padding.
            let value = tag.value.to_string();
            let value = value.trim_end_matches('\0').trim();
            if !value.is_empty() {
                *slot = Some(value.to_string());
            }
        }
    }
}

/// Stream properties fixed at open time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// Total frames when the container declares them (drives progress display).
    pub total_frames: Option<u64>,
    /// Source bit depth when known; leveled output matches it by default.
    pub source_depth: Option<BitDepth>,
}

/// One opened input, ready to stream interleaved f32 chunks.
pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn SymphoniaDecoder>,
    track_id: u32,
    info: InputInfo,
    tags: Tags,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let source = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
            hint.with_extension(ext);
        }

        let mut probed = symphonia::default::get_probe()
            .format(
                &hint,
                source,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .with_context(|| format!("{}: unrecognized or unsupported format", path.display()))?;

        // Tags can sit outside the container (ID3v2 before an MP3) or inside
        // it (Vorbis comments, RIFF INFO); gather both.
        let mut tags = Tags::default();
        if let Some(metadata) = probed.metadata.get()
            && let Some(revision) = metadata.current()
        {
            collect_tags(&mut tags, revision);
        }
        let mut format = probed.format;
        if let Some(revision) = format.metadata().current() {
            collect_tags(&mut tags, revision);
        }

        let track = format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .with_context(|| format!("{}: no decodable audio track", path.display()))?;
        let track_id = track.id;
        let params = track.codec_params.clone();

        let total_frames = params.n_frames;
        // Float sources are named three ways, by demuxer: a float sample
        // format, a float PCM codec (WAV/AIFF say only this), or 32 bits.
        let float_codec = matches!(
            params.codec,
            CODEC_TYPE_PCM_F32LE
                | CODEC_TYPE_PCM_F32BE
                | CODEC_TYPE_PCM_F64LE
                | CODEC_TYPE_PCM_F64BE
        );
        let float_format = matches!(
            params.sample_format,
            Some(SampleFormat::F32 | SampleFormat::F64)
        );
        let source_depth = if float_codec || float_format {
            Some(BitDepth::Float32)
        } else {
            match params.bits_per_sample {
                Some(16) | Some(8) => Some(BitDepth::Int16),
                Some(24) => Some(BitDepth::Int24),
                Some(32) => Some(BitDepth::Float32),
                _ => None,
            }
        };

        let mut decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .with_context(|| format!("{}: no decoder for this codec", path.display()))?;

        // AAC (and friends) may not declare the sample rate or channels until
        // the first frame decodes; peek one, then rewind.
        let known_channels = params
            .channels
            .map(|channels| channels.count())
            .or_else(|| {
                params
                    .channel_layout
                    .map(|layout| layout.into_channels().count())
            });
        let (sample_rate, channels) = match (params.sample_rate, known_channels) {
            (Some(rate), Some(channels)) => (rate, channels),
            (rate, channels) => {
                let (peeked_rate, peeked_channels) =
                    peek_spec(format.as_mut(), decoder.as_mut(), track_id).with_context(|| {
                        format!("{}: cannot determine the stream layout", path.display())
                    })?;
                format
                    .seek(
                        SeekMode::Accurate,
                        SeekTo::Time {
                            time: Time::new(0, 0.0),
                            track_id: Some(track_id),
                        },
                    )
                    .with_context(|| format!("{}: cannot rewind after probing", path.display()))?;
                decoder.reset();
                (
                    rate.unwrap_or(peeked_rate),
                    channels.unwrap_or(peeked_channels),
                )
            }
        };
        if channels == 0 || channels > 2 {
            bail!(
                "{}: {channels} channels; levelize handles mono and stereo only",
                path.display()
            );
        }

        Ok(Decoder {
            format,
            decoder,
            track_id,
            info: InputInfo {
                sample_rate,
                channels: channels as u16,
                total_frames,
                source_depth,
            },
            tags,
        })
    }

    pub fn info(&self) -> &InputInfo {
        &self.info
    }

    /// Basic text tags found in the input.
    pub fn tags(&self) -> &Tags {
        &self.tags
    }

    /// Streams the whole input to `sink` as interleaved f32 chunks of whole
    /// frames; returns the number of frames decoded.
    pub fn stream(mut self, mut sink: impl FnMut(&[f32]) -> Result<()>) -> Result<u64> {
        let mut buffer: Option<SampleBuffer<f32>> = None;
        let mut scratch: Vec<f32> = Vec::new();
        let mut frames: u64 = 0;
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(ref error))
                    if error.kind() == ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(error) => return Err(error).context("reading input"),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = self.decoder.decode(&packet).context("decoding input")?;
            let buffer = buffer.get_or_insert_with(|| {
                SampleBuffer::new(decoded.capacity() as u64, *decoded.spec())
            });
            buffer.copy_interleaved_ref(decoded);
            let mut samples = buffer.samples();
            // A NaN or infinity (broken float WAVs have them) would poison
            // the loudness filters and pass into the output; silence it.
            if samples.iter().any(|s| !s.is_finite()) {
                scratch.clear();
                scratch.extend(samples.iter().map(|s| if s.is_finite() { *s } else { 0.0 }));
                samples = &scratch;
            }
            frames += (samples.len() / usize::from(self.info.channels)) as u64;
            sink(samples)?;
        }
        Ok(frames)
    }
}
