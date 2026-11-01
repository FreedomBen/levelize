# Changelog

Notable changes to levelize. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
semver, with the usual 0.x caveat that minor versions may break.

## [0.0.1] — unreleased

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
- Inputs: WAV, AIFF, FLAC, MP3, AAC/M4A/M4B, Ogg Vorbis; mono and stereo.
  Outputs: WAV, FLAC, MP3 (via `lame`; one MP3 render caps at ~3 h of
  stereo), and M4B/M4A (AAC via `ffmpeg`; no length cap), with basic tags
  copied over.
- Audiobook output: `.m4b` carries the iTunes audiobook marker so players
  resume playback. Chapter marks found in the input (MP4 chapters and ID3
  CHAP alike) carry over automatically; `--chapters` replaces them from a
  plain `TIME Title` text file, `--no-chapters` strips them.
- Lossy outputs default to the input's own measured bitrate when the input
  is itself lossy (`--mp3-bitrate`/`--aac-bitrate` override; lossless inputs
  get 128/96 kbps MP3 and 96/64 kbps AAC for stereo/mono).
- `--analyze` measurement mode, `--json` stats (one object per input),
  stdin/stdout filter mode, progress bar, meaningful exit codes (0/1/2), and
  byte-deterministic output.
- Hardening: non-finite samples (NaN/±inf) are silenced at the decode
  boundary; empty, truncated, and non-audio inputs are handled cleanly; a
  failing input doesn't stop a batch.
- Man page generated from the CLI definition (`make man`), `make install`
  (PREFIX defaults to `/usr/local`), and prebuilt release binaries for Linux
  (static musl, x86_64/aarch64), macOS (universal), and Windows.
