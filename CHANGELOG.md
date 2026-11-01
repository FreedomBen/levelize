# Changelog

Notable changes to levelize. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
semver, with the usual 0.x caveat that minor versions may break.

## [0.0.2] — 2026-11-01

### Added

- M4B/M4A output: AAC in MP4 via `ffmpeg` (its built-in encoder), with no
  length cap. `.m4b` carries the iTunes audiobook marker so players resume
  playback.
- Chapter marks in M4B/M4A output: chapters found in the input (MP4 chapters
  and ID3 CHAP alike) carry over automatically, `--chapters` replaces them
  from a plain `TIME Title` text file, and `--no-chapters` strips them. The
  report and `--json` output count the marks written.

### Changed

- Lossy outputs (MP3, and the new M4B/M4A) default to the input's own
  measured bitrate when the input is itself lossy — snapped to real CBR
  steps for MP3. `--mp3-bitrate`/`--aac-bitrate` still override, and
  lossless inputs keep the static defaults (128/96 kbps MP3, 96/64 kbps
  AAC, for stereo/mono).

## [0.0.1] — 2026-10-31

First release: what The Levelator did for spoken-word audio in 2006, rebuilt
on today's loudness standards as a single fast CLI.

### Added

- Adaptive leveling: BS.1770 short-term loudness drives a smoothed gain curve —
  slew-limited inside speech, larger corrections jumped at pause midpoints —
  so levels even out between and within speakers.
- Program/pause classification anchored to the recording's noise floor, so
  room tone and breaths aren't amplified between sentences.
- Exact integrated-loudness targeting (−16 LUFS stereo podcast default),
  verified against the R128 gate, with `--profile podcast|broadcast|audiobook`
  presets and explicit `--target`/`--ceiling` overrides.
- True-peak limiting: a 4×-oversampled lookahead limiter guarantees the
  ceiling (−1.5 dBTP default) against intersample peaks.
- Inputs: WAV, AIFF, FLAC, MP3, AAC/M4A, Ogg Vorbis; mono and stereo.
  Outputs: WAV, FLAC, and MP3 (via `lame`; one MP3 render caps at ~3 h of
  stereo), with basic tags copied over.
- `--analyze` measurement mode, `--json` stats (one object per input),
  stdin/stdout filter mode, progress bar, meaningful exit codes (0/1/2), and
  byte-deterministic output.
- Hardening: non-finite samples (NaN/±inf) are silenced at the decode
  boundary; empty, truncated, and non-audio inputs are handled cleanly; a
  failing input doesn't stop a batch.
- Man page generated from the CLI definition (`make man`), `make install`
  (PREFIX defaults to `/usr/local`), and prebuilt release binaries for Linux
  (static musl, x86_64/aarch64), macOS (universal), and Windows.
