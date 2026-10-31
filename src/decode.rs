//! Streaming decode of input audio via symphonia (WAV in M1; more formats in M4).

use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, bail};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, Decoder as SymphoniaDecoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::sample::SampleFormat;

use crate::BitDepth;

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
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let source = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                source,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .with_context(|| format!("{}: unrecognized or unsupported format", path.display()))?;
        let format = probed.format;

        let track = format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .with_context(|| format!("{}: no decodable audio track", path.display()))?;
        let track_id = track.id;

        let sample_rate = track
            .codec_params
            .sample_rate
            .with_context(|| format!("{}: sample rate unknown", path.display()))?;
        let channels = track
            .codec_params
            .channels
            .map(|channels| channels.count())
            .with_context(|| format!("{}: channel layout unknown", path.display()))?;
        if channels == 0 || channels > 2 {
            bail!(
                "{}: {channels} channels; levelize handles mono and stereo only",
                path.display()
            );
        }
        let total_frames = track.codec_params.n_frames;
        let source_depth = match (
            track.codec_params.sample_format,
            track.codec_params.bits_per_sample,
        ) {
            (Some(SampleFormat::F32 | SampleFormat::F64), _) => Some(BitDepth::Float32),
            (_, Some(16)) => Some(BitDepth::Int16),
            (_, Some(24)) => Some(BitDepth::Int24),
            (_, Some(32)) => Some(BitDepth::Float32),
            (_, Some(8)) => Some(BitDepth::Int16),
            _ => None,
        };

        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .with_context(|| format!("{}: no decoder for this codec", path.display()))?;

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
        })
    }

    pub fn info(&self) -> &InputInfo {
        &self.info
    }

    /// Streams the whole input to `sink` as interleaved f32 chunks of whole
    /// frames; returns the number of frames decoded.
    pub fn stream(mut self, mut sink: impl FnMut(&[f32]) -> Result<()>) -> Result<u64> {
        let mut buffer: Option<SampleBuffer<f32>> = None;
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
            let samples = buffer.samples();
            frames += (samples.len() / usize::from(self.info.channels)) as u64;
            sink(samples)?;
        }
        Ok(frames)
    }
}
