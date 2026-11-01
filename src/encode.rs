//! Encoders: WAV and FLAC writers, plus MP3 via a `lame` child process and
//! M4B/M4A (AAC in MP4) via an `ffmpeg` child process.
//!
//! Pass 1 fixes the exact frame count, so headers are written up front and no
//! writer ever seeks — pipes work. (For FLAC files, [`patch_flac_header`] can
//! fill in the MD5 and frame-size fields afterwards; on pipes they stay at
//! "unknown", which the spec allows. MP4 is the exception: its muxer must
//! seek, so ffmpeg writes a temp file that is then copied to the sink.) TPDF
//! dither is applied when quantizing to 16-bit, from a fixed-seed generator
//! so runs are byte-identical; the MP3 and AAC paths are fed 32-bit floats,
//! so nothing is quantized before the encoder.

use std::io::{BufWriter, ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow, bail, ensure};
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::error::{Verified, Verify};
use flacenc::source::{Context as FlacContext, Fill, FrameBuf};

use crate::BitDepth;
use crate::decode::Tags;

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
    pub fn new(mut sink: W, spec: WavSpec, total_frames: u64, tags: &Tags) -> Result<Self> {
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
        let info_chunk = riff_info_chunk(tags);
        // WAVE id + fmt chunk + (float only) fact chunk + LIST INFO chunk
        // + data chunk header.
        let overhead: u64 = if is_float {
            4 + 26 + 12 + 8
        } else {
            4 + 24 + 8
        } + info_chunk.len() as u64;
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
        header.extend_from_slice(&info_chunk);
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

/// RIFF `LIST INFO` chunk bytes (empty when there are no tags). Fourccs
/// follow common practice and what symphonia/ffmpeg read back: `IPRT` carries
/// the track number.
fn riff_info_chunk(tags: &Tags) -> Vec<u8> {
    if tags.is_empty() {
        return Vec::new();
    }
    let fields: [(&[u8; 4], &Option<String>); 7] = [
        (b"INAM", &tags.title),
        (b"IART", &tags.artist),
        (b"IPRD", &tags.album),
        (b"ICRD", &tags.date),
        (b"IGNR", &tags.genre),
        (b"ICMT", &tags.comment),
        (b"IPRT", &tags.track),
    ];
    let mut body = Vec::new();
    body.extend_from_slice(b"INFO");
    for (fourcc, value) in fields {
        let Some(value) = value else { continue };
        let text_len = value.len() as u32 + 1; // NUL-terminated
        body.extend_from_slice(fourcc);
        body.extend_from_slice(&text_len.to_le_bytes());
        body.extend_from_slice(value.as_bytes());
        body.push(0);
        if text_len % 2 == 1 {
            body.push(0); // word-align the next subchunk
        }
    }
    let mut chunk = Vec::with_capacity(body.len() + 8);
    chunk.extend_from_slice(b"LIST");
    chunk.extend_from_slice(&(body.len() as u32).to_le_bytes());
    chunk.extend_from_slice(&body);
    chunk
}

/// FLAC frames per encoded block.
const FLAC_BLOCK_FRAMES: usize = 4096;

/// What a finished FLAC stream learned that its already-written header
/// couldn't know; [`patch_flac_header`] writes it back into a seekable file.
pub struct FlacStats {
    pub md5: [u8; 16],
    pub min_frame_size: u32,
    pub max_frame_size: u32,
}

/// Streaming FLAC writer on the same no-seek plan as [`WavWriter`]: the
/// `fLaC` header, STREAMINFO (total samples known from pass 1), and a Vorbis
/// comment block go out up front, then one encoded frame per 4096-frame
/// block.
pub struct FlacWriter<W: Write> {
    sink: W,
    channels: usize,
    bit_depth: BitDepth,
    expected_frames: u64,
    frames_written: u64,
    /// Interleaved i32 samples waiting to fill a block.
    pending: Vec<i32>,
    framebuf: FrameBuf,
    context: FlacContext,
    stream_info: StreamInfo,
    config: Verified<flacenc::config::Encoder>,
    frame_number: usize,
    min_frame_size: u32,
    max_frame_size: u32,
    dither: Tpdf,
}

impl<W: Write> FlacWriter<W> {
    pub fn new(mut sink: W, spec: WavSpec, total_frames: u64, tags: &Tags) -> Result<Self> {
        let bits = match spec.bit_depth {
            BitDepth::Int16 => 16,
            BitDepth::Int24 => 24,
            BitDepth::Float32 => bail!("FLAC output supports bit depths 16 and 24"),
        };
        let channels = usize::from(spec.channels);

        let mut config = flacenc::config::Encoder::default();
        config.block_size = FLAC_BLOCK_FRAMES;
        config.multithread = false;
        let config = config
            .into_verified()
            .map_err(|(_, error)| anyhow!("FLAC encoder configuration: {error}"))?;
        let mut stream_info = StreamInfo::new(spec.sample_rate as usize, channels, bits)
            .map_err(|error| anyhow!("FLAC stream parameters: {error}"))?;
        stream_info
            .set_block_sizes(FLAC_BLOCK_FRAMES, FLAC_BLOCK_FRAMES)
            .map_err(|error| anyhow!("FLAC block size: {error}"))?;
        stream_info.set_total_samples(total_frames as usize);

        // "fLaC", then STREAMINFO (type 0), then a Vorbis comment block
        // (type 4) marked as the last metadata block.
        let mut body = ByteSink::new();
        stream_info
            .write(&mut body)
            .map_err(|error| anyhow!("serializing STREAMINFO: {error}"))?;
        sink.write_all(b"fLaC").context("writing FLAC header")?;
        sink.write_all(&[0]).context("writing FLAC header")?;
        sink.write_all(&(body.as_slice().len() as u32).to_be_bytes()[1..])
            .context("writing FLAC header")?;
        sink.write_all(body.as_slice())
            .context("writing FLAC header")?;
        let comment = vorbis_comment(tags);
        sink.write_all(&[0x84]).context("writing FLAC tags")?;
        sink.write_all(&(comment.len() as u32).to_be_bytes()[1..])
            .context("writing FLAC tags")?;
        sink.write_all(&comment).context("writing FLAC tags")?;

        Ok(FlacWriter {
            sink,
            channels,
            bit_depth: spec.bit_depth,
            expected_frames: total_frames,
            frames_written: 0,
            pending: Vec::new(),
            framebuf: FrameBuf::with_size(channels, FLAC_BLOCK_FRAMES)
                .map_err(|error| anyhow!("FLAC frame buffer: {error}"))?,
            context: FlacContext::new(bits, channels),
            stream_info,
            config,
            frame_number: 0,
            min_frame_size: u32::MAX,
            max_frame_size: 0,
            dither: Tpdf::new(),
        })
    }

    pub fn write_frames(&mut self, interleaved: &[f32]) -> Result<()> {
        debug_assert_eq!(interleaved.len() % self.channels, 0);
        self.frames_written += (interleaved.len() / self.channels) as u64;
        ensure!(
            self.frames_written <= self.expected_frames,
            "more frames than the FLAC header declared"
        );
        match self.bit_depth {
            BitDepth::Int16 => {
                for sample in interleaved {
                    let value = f64::from(*sample) * 32767.0 + self.dither.next();
                    self.pending
                        .push(value.round().clamp(-32768.0, 32767.0) as i32);
                }
            }
            _ => {
                for sample in interleaved {
                    let value = (f64::from(*sample) * 8_388_607.0)
                        .round()
                        .clamp(-8_388_608.0, 8_388_607.0) as i32;
                    self.pending.push(value);
                }
            }
        }
        let block_samples = FLAC_BLOCK_FRAMES * self.channels;
        while self.pending.len() >= block_samples {
            self.framebuf
                .fill_interleaved(&self.pending[..block_samples])
                .map_err(|error| anyhow!("FLAC block: {error}"))?;
            self.context
                .fill_interleaved(&self.pending[..block_samples])
                .map_err(|error| anyhow!("FLAC block: {error}"))?;
            self.emit_frame()?;
            self.pending.drain(..block_samples);
        }
        Ok(())
    }

    fn emit_frame(&mut self) -> Result<()> {
        let frame = flacenc::encode_fixed_size_frame(
            &self.config,
            &self.framebuf,
            self.frame_number,
            &self.stream_info,
        )
        .map_err(|error| anyhow!("encoding FLAC frame: {error}"))?;
        self.frame_number += 1;
        let mut bytes = ByteSink::new();
        frame
            .write(&mut bytes)
            .map_err(|error| anyhow!("serializing FLAC frame: {error}"))?;
        let size = bytes.as_slice().len() as u32;
        self.min_frame_size = self.min_frame_size.min(size);
        self.max_frame_size = self.max_frame_size.max(size);
        self.sink
            .write_all(bytes.as_slice())
            .context("writing FLAC data")
    }

    pub fn finish(mut self) -> Result<FlacStats> {
        ensure!(
            self.frames_written == self.expected_frames,
            "wrote {} frames but the FLAC header declared {}",
            self.frames_written,
            self.expected_frames
        );
        if !self.pending.is_empty() {
            self.framebuf
                .fill_interleaved(&self.pending)
                .map_err(|error| anyhow!("FLAC block: {error}"))?;
            self.context
                .fill_interleaved(&self.pending)
                .map_err(|error| anyhow!("FLAC block: {error}"))?;
            self.emit_frame()?;
            self.pending.clear();
        }
        self.sink.flush().context("flushing FLAC output")?;
        let any_frames = self.frame_number > 0;
        Ok(FlacStats {
            md5: self.context.md5_digest(),
            min_frame_size: if any_frames { self.min_frame_size } else { 0 },
            max_frame_size: self.max_frame_size,
        })
    }
}

/// FLAC `VORBIS_COMMENT` block payload: vendor string plus the tags.
fn vorbis_comment(tags: &Tags) -> Vec<u8> {
    let fields: [(&str, &Option<String>); 7] = [
        ("TITLE", &tags.title),
        ("ARTIST", &tags.artist),
        ("ALBUM", &tags.album),
        ("DATE", &tags.date),
        ("GENRE", &tags.genre),
        ("COMMENT", &tags.comment),
        ("TRACKNUMBER", &tags.track),
    ];
    let vendor = concat!("levelize ", env!("CARGO_PKG_VERSION"));
    let mut payload = Vec::new();
    payload.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    payload.extend_from_slice(vendor.as_bytes());
    let present = fields.iter().filter(|(_, value)| value.is_some()).count();
    payload.extend_from_slice(&(present as u32).to_le_bytes());
    for (name, value) in fields {
        let Some(value) = value else { continue };
        let entry = format!("{name}={value}");
        payload.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        payload.extend_from_slice(entry.as_bytes());
    }
    payload
}

/// Fill in the STREAMINFO fields only known once the stream is finished.
/// Our header layout is fixed: `fLaC` (4) + block header (4) + STREAMINFO
/// body, whose min/max frame sizes sit at body offset 4 and MD5 at offset 18.
pub fn patch_flac_header(path: &Path, stats: &FlacStats) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("reopening {} to finish its header", path.display()))?;
    file.seek(SeekFrom::Start(12))?;
    file.write_all(&stats.min_frame_size.to_be_bytes()[1..])?;
    file.write_all(&stats.max_frame_size.to_be_bytes()[1..])?;
    file.seek(SeekFrom::Start(26))?;
    file.write_all(&stats.md5)?;
    Ok(())
}

/// Fail fast when `lame` is missing, before any decoding work happens.
pub fn check_lame() -> Result<()> {
    match Command::new("lame")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            bail!("MP3 output needs the `lame` encoder on PATH")
        }
        Err(error) => Err(error).context("running lame"),
    }
}

/// MP3 via `lame`: 32-bit float WAV streams into the child's stdin (so
/// nothing is quantized on the way), and the encoded MP3 is pumped from its
/// stdout to the sink on a small I/O thread.
pub struct Mp3Writer {
    wav: WavWriter<ChildStdin>,
    child: Child,
    pump: JoinHandle<std::io::Result<()>>,
}

impl Mp3Writer {
    pub fn new(
        mut sink: Box<dyn Write + Send>,
        sample_rate: u32,
        channels: u16,
        total_frames: u64,
        bitrate_kbps: u32,
        tags: &Tags,
    ) -> Result<Self> {
        // The float-WAV pipe into lame carries RIFF's 4 GB cap; say so up
        // front instead of failing mid-render with a WAV-shaped error.
        let pipe_bytes = total_frames
            .checked_mul(u64::from(channels) * 4)
            .and_then(|data| data.checked_add(50)); // float-WAV header overhead
        ensure!(
            pipe_bytes.is_some_and(|bytes| bytes <= u64::from(u32::MAX)),
            "input too long for MP3 output: the WAV stream feeding lame would pass 4 GB \
             (about 3 hours of stereo at 48 kHz); write WAV or FLAC instead"
        );
        let mut command = Command::new("lame");
        command
            .arg("--silent")
            .arg("-b")
            .arg(bitrate_kbps.to_string())
            .arg("--cbr");
        if !tags.is_empty() {
            command.arg("--add-id3v2");
            let fields = [
                ("--tt", &tags.title),
                ("--ta", &tags.artist),
                ("--tl", &tags.album),
                ("--tg", &tags.genre),
                ("--tc", &tags.comment),
                ("--tn", &tags.track),
            ];
            for (flag, value) in fields {
                if let Some(value) = value {
                    // NUL would be rejected as a process argument.
                    command.arg(flag).arg(value.replace('\0', ""));
                }
            }
            // `--ty` wants a year, not a full date.
            if let Some(year) = tags.date.as_deref().map(leading_year)
                && !year.is_empty()
            {
                command.arg("--ty").arg(year);
            }
        }
        command
            .arg("-")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                bail!("MP3 output needs the `lame` encoder on PATH")
            }
            Err(error) => return Err(error).context("starting lame"),
        };
        let stdin = child.stdin.take().expect("stdin was piped");
        let mut stdout = child.stdout.take().expect("stdout was piped");
        let pump = std::thread::spawn(move || -> std::io::Result<()> {
            std::io::copy(&mut stdout, &mut sink)?;
            sink.flush()
        });
        let spec = WavSpec {
            sample_rate,
            channels,
            bit_depth: BitDepth::Float32,
        };
        let wav = WavWriter::new(stdin, spec, total_frames, &Tags::default())?;
        Ok(Mp3Writer { wav, child, pump })
    }

    pub fn write_frames(&mut self, interleaved: &[f32]) -> Result<()> {
        self.wav.write_frames(interleaved)
    }

    pub fn finish(self) -> Result<()> {
        let Mp3Writer {
            wav,
            mut child,
            pump,
        } = self;
        wav.finish()?; // consumes the writer, closing lame's stdin
        let status = child.wait().context("waiting for lame")?;
        pump.join()
            .map_err(|_| anyhow!("MP3 output thread panicked"))?
            .context("writing MP3 output")?;
        ensure!(status.success(), "lame failed ({status})");
        Ok(())
    }
}

/// Leading run of ASCII digits, at most four — "2026-05-01" → "2026".
fn leading_year(date: &str) -> &str {
    let digits = date.bytes().take_while(u8::is_ascii_digit).count().min(4);
    &date[..digits]
}

pub fn check_ffmpeg() -> Result<()> {
    match Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            bail!("M4B/M4A output needs `ffmpeg` on PATH")
        }
        Err(error) => Err(error).context("running ffmpeg"),
    }
}

/// One chapter mark for M4B/M4A output.
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub start_secs: f64,
    pub title: String,
}

/// Parse a chapters file: one `TIME Title` per line, where TIME is
/// `[[H:]M:]S[.sss]`. Blank lines and `#` comments are skipped; times must
/// increase.
pub fn load_chapters(path: &Path) -> Result<Vec<Chapter>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read chapters file {}", path.display()))?;
    let mut chapters: Vec<Chapter> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |what: &str| format!("{}:{}: {what}", path.display(), index + 1);
        let Some((stamp, title)) = line.split_once(char::is_whitespace) else {
            bail!("{}", at("expected `TIME Title`"));
        };
        let Some(start_secs) = parse_timestamp(stamp) else {
            bail!("{}", at("bad time; use [H:]MM:SS[.sss] or plain seconds"));
        };
        let title = title.trim();
        if title.is_empty() {
            bail!("{}", at("chapter title is empty"));
        }
        if let Some(last) = chapters.last()
            && start_secs <= last.start_secs
        {
            bail!("{}", at("chapter times must increase"));
        }
        chapters.push(Chapter {
            start_secs,
            title: title.to_string(),
        });
    }
    ensure!(
        !chapters.is_empty(),
        "{}: no chapters found",
        path.display()
    );
    Ok(chapters)
}

/// `[[H:]M:]S[.sss]` → seconds. Only the last part may be fractional, and
/// parts after the first must stay below 60.
fn parse_timestamp(stamp: &str) -> Option<f64> {
    let parts: Vec<&str> = stamp.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut secs = 0.0;
    for (index, part) in parts.iter().enumerate() {
        let last = index == parts.len() - 1;
        let digits = |c: char| c.is_ascii_digit() || (last && c == '.');
        if part.is_empty() || !part.chars().all(digits) {
            return None;
        }
        let value: f64 = part.parse().ok()?;
        if index > 0 && value >= 60.0 {
            return None;
        }
        secs = secs * 60.0 + value;
    }
    Some(secs)
}

/// Serialize chapters in ffmpeg's ffmetadata format. Each chapter ends where
/// the next begins; the last ends at the duration.
fn ffmetadata_chapters(chapters: &[Chapter], duration_secs: f64) -> String {
    fn escape(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for c in text.chars() {
            if matches!(c, '=' | ';' | '#' | '\\' | '\n') {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        escaped
    }
    let mut meta = String::from(";FFMETADATA1\n");
    for (index, chapter) in chapters.iter().enumerate() {
        let end_secs = chapters
            .get(index + 1)
            .map_or(duration_secs, |next| next.start_secs);
        meta.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle={}\n",
            (chapter.start_secs * 1000.0).round() as u64,
            (end_secs * 1000.0).round() as u64,
            escape(&chapter.title)
        ));
    }
    meta
}

/// The sample rates AAC (and ffmpeg's encoder) can represent.
const AAC_SAMPLE_RATES: [u32; 13] = [
    7_350, 8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 64_000, 88_200,
    96_000,
];

/// Fixed parameters for one M4B/M4A render.
#[derive(Debug, Clone, Copy)]
pub struct AacSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub total_frames: u64,
    pub bitrate_kbps: u32,
    /// Mark the file as an audiobook (the iTunes `stik` atom, as in .m4b).
    pub audiobook: bool,
}

/// AAC in MP4 (M4B/M4A) via `ffmpeg`: raw 32-bit float samples stream into
/// the child's stdin — no WAV framing, so no 4 GB cap — and ffmpeg muxes
/// into a temp file (MP4 needs a seekable output), which [`AacWriter::finish`]
/// copies to the sink.
pub struct AacWriter {
    sink: Box<dyn Write + Send>,
    stdin: BufWriter<ChildStdin>,
    child: Child,
    /// Owns the muxer's output until it is copied out; deleted on drop.
    tmp: tempfile::TempDir,
    out_path: PathBuf,
    scratch: Vec<u8>,
    channels: u16,
    frames_written: u64,
    total_frames: u64,
}

impl AacWriter {
    pub fn new(
        sink: Box<dyn Write + Send>,
        spec: AacSpec,
        tags: &Tags,
        chapters: &[Chapter],
    ) -> Result<Self> {
        ensure!(
            AAC_SAMPLE_RATES.contains(&spec.sample_rate),
            "AAC supports standard sample rates up to 96 kHz, not {} Hz; write WAV or FLAC instead",
            spec.sample_rate
        );
        let duration_secs = spec.total_frames as f64 / f64::from(spec.sample_rate);
        if let Some(last) = chapters.last() {
            ensure!(
                last.start_secs < duration_secs,
                "chapter \"{}\" starts at {:.3} s, past the end of the audio ({:.3} s)",
                last.title,
                last.start_secs,
                duration_secs
            );
        }
        let tmp = tempfile::TempDir::with_prefix("levelize-mux-")
            .context("creating a temp dir for the MP4 muxer")?;
        let out_path = tmp
            .path()
            .join(if spec.audiobook { "out.m4b" } else { "out.m4a" });

        let mut command = Command::new("ffmpeg");
        command.args(["-hide_banner", "-loglevel", "error", "-f", "f32le"]);
        command.arg("-ar").arg(spec.sample_rate.to_string());
        command.arg("-ac").arg(spec.channels.to_string());
        command.args(["-i", "-"]);
        if !chapters.is_empty() {
            let meta_path = tmp.path().join("chapters.ffmeta");
            std::fs::write(&meta_path, ffmetadata_chapters(chapters, duration_secs))
                .context("writing the chapter metadata file")?;
            command.args(["-f", "ffmetadata", "-i"]).arg(&meta_path);
            command.args(["-map", "0:a", "-map_chapters", "1"]);
        }
        command
            .args(["-c:a", "aac", "-b:a"])
            .arg(format!("{}k", spec.bitrate_kbps));
        let fields = [
            ("title", &tags.title),
            ("artist", &tags.artist),
            ("album", &tags.album),
            ("date", &tags.date),
            ("genre", &tags.genre),
            ("comment", &tags.comment),
            ("track", &tags.track),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                // NUL would be rejected as a process argument.
                command
                    .arg("-metadata")
                    .arg(format!("{key}={}", value.replace('\0', "")));
            }
        }
        if spec.audiobook {
            // The iTunes media-type atom: 2 = audiobook (players resume).
            command.args(["-metadata", "media_type=2"]);
        }
        // faststart fronts the index; bitexact keeps runs byte-identical.
        command.args([
            "-movflags",
            "+faststart",
            "-fflags",
            "+bitexact",
            "-flags",
            "+bitexact",
        ]);
        command.args(["-f", "ipod"]).arg(&out_path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                bail!("M4B/M4A output needs `ffmpeg` on PATH")
            }
            Err(error) => return Err(error).context("starting ffmpeg"),
        };
        let stdin = BufWriter::new(child.stdin.take().expect("stdin was piped"));
        Ok(AacWriter {
            sink,
            stdin,
            child,
            tmp,
            out_path,
            scratch: Vec::new(),
            channels: spec.channels,
            frames_written: 0,
            total_frames: spec.total_frames,
        })
    }

    pub fn write_frames(&mut self, interleaved: &[f32]) -> Result<()> {
        self.scratch.clear();
        self.scratch.reserve(interleaved.len() * 4);
        for sample in interleaved {
            self.scratch.extend_from_slice(&sample.to_le_bytes());
        }
        self.stdin
            .write_all(&self.scratch)
            .context("feeding ffmpeg")?;
        self.frames_written += (interleaved.len() / usize::from(self.channels)) as u64;
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        let AacWriter {
            mut sink,
            stdin,
            mut child,
            tmp: _tmp, // outlives the copy below
            out_path,
            frames_written,
            total_frames,
            ..
        } = self;
        ensure!(
            frames_written == total_frames,
            "AAC writer closed with {frames_written} of {total_frames} frames"
        );
        // Closing stdin tells ffmpeg the stream is done.
        drop(
            stdin
                .into_inner()
                .map_err(|error| anyhow!("flushing ffmpeg's stdin: {}", error.error()))?,
        );
        let status = child.wait().context("waiting for ffmpeg")?;
        ensure!(status.success(), "ffmpeg failed ({status})");
        let mut muxed = std::fs::File::open(&out_path).context("reading the muxed MP4 back")?;
        std::io::copy(&mut muxed, &mut sink).context("writing M4B/M4A output")?;
        sink.flush().context("writing M4B/M4A output")?;
        Ok(())
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
        let mut writer = WavWriter::new(sink, spec, frames, &Tags::default()).unwrap();
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
        let mut writer = WavWriter::new(Vec::new(), spec, 10, &Tags::default()).unwrap();
        writer.write_frames(&[0.0; 10]).unwrap(); // 5 of 10 frames
        assert!(writer.finish().is_err());
    }

    #[test]
    fn riff_info_subchunks_are_word_aligned() {
        let tags = Tags {
            title: Some("Odd".into()),   // 3 bytes + NUL = even, no pad
            artist: Some("Even".into()), // 4 bytes + NUL = odd, padded
            ..Tags::default()
        };
        let chunk = riff_info_chunk(&tags);
        assert_eq!(&chunk[..4], b"LIST");
        assert_eq!(chunk.len() % 2, 0);
        let declared = u32::from_le_bytes(chunk[4..8].try_into().unwrap()) as usize;
        assert_eq!(declared, chunk.len() - 8);
        assert_eq!(&chunk[8..12], b"INFO");
        assert_eq!(&chunk[12..16], b"INAM");
        assert!(riff_info_chunk(&Tags::default()).is_empty());
    }

    #[test]
    fn vorbis_comment_counts_only_present_tags() {
        let tags = Tags {
            title: Some("T".into()),
            track: Some("7".into()),
            ..Tags::default()
        };
        let payload = vorbis_comment(&tags);
        let vendor_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
        let count_at = 4 + vendor_len;
        let count = u32::from_le_bytes(payload[count_at..count_at + 4].try_into().unwrap());
        assert_eq!(count, 2);
        let text = String::from_utf8_lossy(&payload);
        assert!(text.contains("TITLE=T"));
        assert!(text.contains("TRACKNUMBER=7"));
    }

    #[test]
    fn flac_output_roundtrips_through_symphonia() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.flac");
        // Longer than one 4096-frame block, with a partial tail block.
        let samples = ramp(2 * 5000);
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 2,
            bit_depth: BitDepth::Int24,
        };
        let frames = (samples.len() / 2) as u64;
        let sink = BufWriter::new(File::create(&path).unwrap());
        let mut writer = FlacWriter::new(sink, spec, frames, &Tags::default()).unwrap();
        for chunk in samples.chunks(999 * 2) {
            writer.write_frames(chunk).unwrap();
        }
        let stats = writer.finish().unwrap();
        assert!(stats.min_frame_size > 0);
        assert!(stats.max_frame_size >= stats.min_frame_size);

        let (rate, channels, decoded) = read_back(&path);
        assert_eq!(rate, 48_000);
        assert_eq!(channels, 2);
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(&decoded) {
            assert!((a - b).abs() <= 2.0 / 8_388_607.0, "{a} vs {b}");
        }
    }

    #[test]
    fn leading_year_strips_month_and_day() {
        assert_eq!(leading_year("2026-05-01"), "2026");
        assert_eq!(leading_year("2026"), "2026");
        assert_eq!(leading_year("May 2026"), "");
    }

    #[test]
    fn mp3_overflow_is_rejected_before_spawning_lame() {
        // Stereo float frames past the RIFF cap: 4 GB / 8 bytes per frame.
        let frames = (u64::from(u32::MAX) - 50) / 8 + 1;
        let error = Mp3Writer::new(
            Box::new(Vec::new()),
            48_000,
            2,
            frames,
            128,
            &Tags::default(),
        )
        .err()
        .map(|error| error.to_string())
        .expect("an over-long input must be rejected");
        assert!(error.contains("MP3 output"), "{error}");
        assert!(error.contains("WAV or FLAC"), "{error}");

        // u64 arithmetic on absurd counts must not wrap past the check.
        assert!(
            Mp3Writer::new(
                Box::new(Vec::new()),
                48_000,
                2,
                u64::MAX,
                128,
                &Tags::default()
            )
            .is_err()
        );
    }

    #[test]
    fn chapters_parse_times_and_titles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("toc.txt");
        std::fs::write(
            &path,
            "# a comment\n\n0 Intro\n1:05 Middle part\n01:02:03.5 The End\n",
        )
        .unwrap();
        let chapters = load_chapters(&path).unwrap();
        assert_eq!(
            chapters,
            [
                Chapter {
                    start_secs: 0.0,
                    title: "Intro".into()
                },
                Chapter {
                    start_secs: 65.0,
                    title: "Middle part".into()
                },
                Chapter {
                    start_secs: 3723.5,
                    title: "The End".into()
                },
            ]
        );
    }

    #[test]
    fn chapters_reject_disorder_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("toc.txt");
        let error_for = |text: &str| {
            std::fs::write(&path, text).unwrap();
            load_chapters(&path).unwrap_err().to_string()
        };
        let error = error_for("10 A\n5 B\n");
        assert!(
            error.contains(":2:") && error.contains("must increase"),
            "{error}"
        );
        let error = error_for("eleven A\n");
        assert!(error.contains("bad time"), "{error}");
        let error = error_for("1:99 Overflowed seconds\n");
        assert!(error.contains("bad time"), "{error}");
        let error = error_for("5\n");
        assert!(error.contains("TIME Title"), "{error}");
        let error = error_for("# nothing but comments\n");
        assert!(error.contains("no chapters"), "{error}");
    }

    #[test]
    fn ffmetadata_escapes_and_chains_ends() {
        let chapters = [
            Chapter {
                start_secs: 0.0,
                title: "A = B; #1 \\".into(),
            },
            Chapter {
                start_secs: 2.5,
                title: "Two".into(),
            },
        ];
        let meta = ffmetadata_chapters(&chapters, 5.0);
        assert!(meta.starts_with(";FFMETADATA1\n"), "{meta}");
        assert!(
            meta.contains("START=0\nEND=2500\ntitle=A \\= B\\; \\#1 \\\\\n"),
            "{meta}"
        );
        assert!(meta.contains("START=2500\nEND=5000\ntitle=Two\n"), "{meta}");
    }

    #[test]
    fn aac_writer_rejects_bad_rates_and_late_chapters_before_spawning() {
        let spec = AacSpec {
            sample_rate: 192_000,
            channels: 2,
            total_frames: 192_000,
            bitrate_kbps: 96,
            audiobook: true,
        };
        let error = AacWriter::new(Box::new(std::io::sink()), spec, &Tags::default(), &[])
            .err()
            .map(|error| error.to_string())
            .expect("a non-AAC rate must be rejected");
        assert!(error.contains("96 kHz"), "{error}");

        let spec = AacSpec {
            sample_rate: 48_000,
            total_frames: 48_000, // one second
            ..spec
        };
        let late = [Chapter {
            start_secs: 2.0,
            title: "Late".into(),
        }];
        let error = AacWriter::new(Box::new(std::io::sink()), spec, &Tags::default(), &late)
            .err()
            .map(|error| error.to_string())
            .expect("a chapter past the end must be rejected");
        assert!(error.contains("past the end"), "{error}");
    }
}
