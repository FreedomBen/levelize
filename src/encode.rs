//! WAV output. Pass 1 fixes the exact frame count, so headers are written up
//! front and the writer never seeks — pipes work. TPDF dither is applied when
//! quantizing to 16-bit, from a fixed-seed generator so runs are byte-identical.

use std::io::Write;

use anyhow::{Context, Result, ensure};

use crate::BitDepth;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub bit_depth: BitDepth,
}

pub struct WavWriter<W: Write> {
    sink: W,
    spec: WavSpec,
    expected_frames: u64,
    frames_written: u64,
    pad_byte: bool,
    dither: Tpdf,
    buf: Vec<u8>,
}

impl<W: Write> WavWriter<W> {
    pub fn new(mut sink: W, spec: WavSpec, total_frames: u64) -> Result<Self> {
        let bytes_per_sample: u64 = match spec.bit_depth {
            BitDepth::Int16 => 2,
            BitDepth::Int24 => 3,
            BitDepth::Float32 => 4,
        };
        let block_align = u64::from(spec.channels) * bytes_per_sample;
        let data_len = total_frames
            .checked_mul(block_align)
            .context("output size overflows")?;
        let pad_byte = data_len % 2 == 1;
        let is_float = spec.bit_depth == BitDepth::Float32;
        // WAVE id + fmt chunk + (float only) fact chunk + data chunk header.
        let overhead: u64 = if is_float {
            4 + 26 + 12 + 8
        } else {
            4 + 24 + 8
        };
        let riff_size = overhead + data_len + u64::from(pad_byte);
        ensure!(
            riff_size <= u64::from(u32::MAX),
            "output would exceed WAV's 4 GB limit"
        );

        let mut header = Vec::with_capacity(70);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(riff_size as u32).to_le_bytes());
        header.extend_from_slice(b"WAVE");
        header.extend_from_slice(b"fmt ");
        if is_float {
            header.extend_from_slice(&18u32.to_le_bytes());
            header.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        } else {
            header.extend_from_slice(&16u32.to_le_bytes());
            header.extend_from_slice(&1u16.to_le_bytes()); // PCM
        }
        header.extend_from_slice(&spec.channels.to_le_bytes());
        header.extend_from_slice(&spec.sample_rate.to_le_bytes());
        header
            .extend_from_slice(&((u64::from(spec.sample_rate) * block_align) as u32).to_le_bytes());
        header.extend_from_slice(&(block_align as u16).to_le_bytes());
        header.extend_from_slice(&((bytes_per_sample * 8) as u16).to_le_bytes());
        if is_float {
            header.extend_from_slice(&0u16.to_le_bytes()); // cbSize
            header.extend_from_slice(b"fact");
            header.extend_from_slice(&4u32.to_le_bytes());
            header.extend_from_slice(&(total_frames as u32).to_le_bytes());
        }
        header.extend_from_slice(b"data");
        header.extend_from_slice(&(data_len as u32).to_le_bytes());
        sink.write_all(&header).context("writing WAV header")?;

        Ok(WavWriter {
            sink,
            spec,
            expected_frames: total_frames,
            frames_written: 0,
            pad_byte,
            dither: Tpdf::new(),
            buf: Vec::new(),
        })
    }

    pub fn write_frames(&mut self, interleaved: &[f32]) -> Result<()> {
        let channels = usize::from(self.spec.channels);
        debug_assert_eq!(interleaved.len() % channels, 0);
        self.frames_written += (interleaved.len() / channels) as u64;
        ensure!(
            self.frames_written <= self.expected_frames,
            "more frames than the WAV header declared"
        );
        self.buf.clear();
        match self.spec.bit_depth {
            BitDepth::Int16 => {
                for sample in interleaved {
                    let value = f64::from(*sample) * 32767.0 + self.dither.next();
                    let quantized = value.round().clamp(-32768.0, 32767.0) as i16;
                    self.buf.extend_from_slice(&quantized.to_le_bytes());
                }
            }
            BitDepth::Int24 => {
                for sample in interleaved {
                    let value = (f64::from(*sample) * 8_388_607.0)
                        .round()
                        .clamp(-8_388_608.0, 8_388_607.0) as i32;
                    self.buf.extend_from_slice(&value.to_le_bytes()[..3]);
                }
            }
            BitDepth::Float32 => {
                for sample in interleaved {
                    self.buf.extend_from_slice(&sample.to_le_bytes());
                }
            }
        }
        self.sink.write_all(&self.buf).context("writing WAV data")
    }

    pub fn finish(mut self) -> Result<()> {
        ensure!(
            self.frames_written == self.expected_frames,
            "wrote {} frames but the WAV header declared {}",
            self.frames_written,
            self.expected_frames
        );
        if self.pad_byte {
            self.sink.write_all(&[0]).context("writing WAV pad byte")?;
        }
        self.sink.flush().context("flushing WAV output")
    }
}

/// TPDF dither in (−1, 1) LSB from a fixed-seed xorshift64*; deterministic so
/// identical runs produce identical bytes.
struct Tpdf {
    state: u64,
}

impl Tpdf {
    fn new() -> Self {
        Tpdf {
            state: 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn unit(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let value = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (value >> 11) as f64 / (1u64 << 53) as f64
    }

    fn next(&mut self) -> f64 {
        self.unit() - self.unit()
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::BufWriter;
    use std::path::Path;

    use super::*;
    use crate::decode::Decoder;

    fn write_file(path: &Path, spec: WavSpec, samples: &[f32]) {
        let frames = (samples.len() / usize::from(spec.channels)) as u64;
        let sink = BufWriter::new(File::create(path).unwrap());
        let mut writer = WavWriter::new(sink, spec, frames).unwrap();
        writer.write_frames(samples).unwrap();
        writer.finish().unwrap();
    }

    fn read_back(path: &Path) -> (u32, u16, Vec<f32>) {
        let decoder = Decoder::open(path).unwrap();
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

    fn ramp(len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| ((i as f32 / len as f32) * 1.6 - 0.8).sin() * 0.9)
            .collect()
    }

    #[test]
    fn float_output_roundtrips_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f32.wav");
        let samples = ramp(2 * 480);
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 2,
            bit_depth: BitDepth::Float32,
        };
        write_file(&path, spec, &samples);
        let (rate, channels, decoded) = read_back(&path);
        assert_eq!(rate, 48_000);
        assert_eq!(channels, 2);
        assert_eq!(decoded, samples);
    }

    #[test]
    fn int16_output_roundtrips_within_dithered_lsb() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i16.wav");
        let samples = ramp(2 * 480);
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 2,
            bit_depth: BitDepth::Int16,
        };
        write_file(&path, spec, &samples);
        let (_, _, decoded) = read_back(&path);
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(&decoded) {
            assert!((a - b).abs() <= 2.0 / 32767.0, "{a} vs {b}");
        }
    }

    #[test]
    fn int24_mono_with_odd_data_length_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i24.wav");
        let samples = ramp(481); // odd byte count: 481 × 3
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 1,
            bit_depth: BitDepth::Int24,
        };
        write_file(&path, spec, &samples);
        let (_, channels, decoded) = read_back(&path);
        assert_eq!(channels, 1);
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(&decoded) {
            assert!((a - b).abs() <= 2.0 / 8_388_607.0, "{a} vs {b}");
        }
    }

    #[test]
    fn finishing_with_missing_frames_fails() {
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 2,
            bit_depth: BitDepth::Int16,
        };
        let mut writer = WavWriter::new(Vec::new(), spec, 10).unwrap();
        writer.write_frames(&[0.0; 10]).unwrap(); // 5 of 10 frames
        assert!(writer.finish().is_err());
    }
}
