# levelize

Adaptive loudness leveling for spoken-word audio, as a single fast CLI.

`levelize` does for podcasts and other speech recordings what The Levelator did in
2006, rebuilt on today's standards: it evens out levels *between and within
speakers* — quiet voices up, loud voices down, pauses and room tone left alone —
then lands the result exactly on a standard integrated-loudness target under a
true-peak ceiling. A static normalizer (`ffmpeg loudnorm`, ReplayGain) moves the
whole file by one gain; `levelize` plans a gain curve over time, so a guest
recorded 12 dB quieter than the host comes out at the same loudness.

```console
$ levelize episode.wav
episode.wav → episode.leveled.wav
  integrated   -15.1 → -16.0 LUFS   (gain +14.1 dB)
  gain range   -0.9 … +14.5 dB
  true peak    -15.0 → -15.5 dBTP
```

## Highlights

- **Speaker-aware leveling.** BS.1770 short-term loudness drives a smoothed gain
  curve: slew-limited to ~2 dB/s inside speech (inaudible drift), with larger
  corrections jumped at pause midpoints, where a gain step can't be heard.
- **Pause-aware.** Program/pause classification anchored to the recording's noise
  floor keeps room tone and breaths from being amplified between sentences — the
  classic AGC failure.
- **Exactly on target.** The post-leveling loudness is predicted analytically and
  trimmed, so output lands on the target (−16 LUFS stereo podcast by default),
  not merely near it, verified against the R128 gate.
- **True-peak safe.** A 4×-oversampled lookahead limiter guarantees the ceiling
  (−1.5 dBTP by default) against intersample peaks.
- **Formats.** Reads WAV, AIFF, FLAC, MP3, AAC/M4A/M4B, and Ogg Vorbis; writes WAV,
  FLAC, MP3 (via the `lame` binary; one MP3 render caps at about 3 hours of
  stereo), and M4B/M4A (AAC via the `ffmpeg` binary; no length cap). Basic tags
  (title, artist, album, date, genre, comment, track) are copied over, and
  lossy output defaults to a lossy input's own measured bitrate.
- **Audiobooks.** `-o book.m4b` writes an audiobook players resume. Chapter
  marks already in the input — MP4 chapters or ID3 chapters in a podcast
  MP3 — carry over automatically; `--chapters toc.txt` replaces them from a
  plain text file (one `TIME Title` line each, like `1:02:30 Part Two`) and
  `--no-chapters` strips them.
- **Scriptable.** `--json` stats (one object per input), stdin/stdout filter
  mode, meaningful exit codes, and byte-deterministic output: the same input and
  settings produce the same file every time.

## Installation

Prebuilt binaries for Linux (static musl, x86_64/aarch64), macOS (universal),
and Windows are on the
[releases page](https://github.com/FreedomBen/levelize/releases); each archive
carries the binary and the man page. With a Rust toolchain (edition 2024,
rustc 1.85 or newer):

```bash
cargo install levelize
```

Or from a checkout, including the man page (PREFIX defaults to `/usr/local`):

```bash
git clone https://github.com/FreedomBen/levelize.git
cd levelize
sudo make install
```

MP3 *output* additionally needs the `lame` encoder on `PATH` at run time, and
M4B/M4A output needs `ffmpeg`; everything else is pure Rust with no system
dependencies.

## Usage

```bash
levelize episode.wav                      # → episode.leveled.wav at -16 LUFS
levelize ep1.wav ep2.flac ep3.mp3         # each input to its own output
levelize --profile audiobook book.flac    # ACX-friendly: -20 LUFS, -3 dBTP
levelize --target -18 --ceiling -2 in.wav # explicit numbers beat the profile
levelize ep.mp3 -o ep.leveled.mp3         # MP3 in and out
levelize book.flac -o book.m4b \
         --chapters toc.txt               # M4B audiobook with chapter marks
levelize --analyze *.wav                  # measure only; write nothing
levelize --json ep.wav | jq .gain         # machine-readable stats
cat ep.wav | levelize - -o - > out.wav    # filter mode: stdin to stdout
```

### Profiles

`--profile` picks the loudness target and true-peak ceiling; explicit `--target`
or `--ceiling` values always win.

| Profile     | Target (stereo) | Target (mono) | Ceiling   | Intended spec  |
| ----------- | --------------- | ------------- | --------- | -------------- |
| `podcast`   | −16 LUFS        | −19 LUFS      | −1.5 dBTP | Apple Podcasts |
| `broadcast` | −23 LUFS        | −23 LUFS      | −1 dBTP   | EBU R128       |
| `audiobook` | −20 LUFS        | −20 LUFS      | −3 dBTP   | ACX            |

### The knobs that matter

| Flag                | Default      | What it does                                           |
| ------------------- | ------------ | ------------------------------------------------------ |
| `--leveling <mode>` | `normal`     | `gentle` \| `normal` \| `aggressive` — how hard levels converge |
| `--max-boost <db>`  | 15           | Cap on upward gain; protects the noise floor           |
| `--max-cut <db>`    | 15           | Cap on downward gain                                   |
| `--bit-depth <d>`   | match input  | `16` \| `24` \| `32f` for WAV; `16` \| `24` for FLAC   |
| `--mp3-bitrate <k>` | input, else 128/96 | CBR kbps for MP3 (stereo/mono default)           |
| `--aac-bitrate <k>` | input, else 96/64  | kbps for M4B/M4A (stereo/mono default)           |
| `--chapters <file>` | input's chapters | Chapter marks for M4B/M4A, one `TIME Title` per line |
| `--no-chapters`     | off          | Don't carry the input's chapter marks into M4B/M4A     |
| `--force`           | off          | Overwrite an existing output file                      |

Run `levelize --help` for the full set. Exit codes: 0 success, 1 runtime
failure, 2 usage error.

## How it works

Two streaming passes; nothing holds the whole file in memory. Pass 1 decodes and
measures BS.1770 loudness on 100 ms hops, classifies each hop as program or
pause, plans a clamped gain curve from the short-term loudness, smooths it, and
trims it so the predicted integrated loudness lands on target. Pass 2 decodes
again, applies the curve with dB-linear interpolation, holds the true-peak
ceiling with a lookahead limiter, adds TPDF dither where 16-bit output is
quantized, and encodes. The full design, including the measurement math and the
acceptance invariants the tests enforce, is in [DESIGN.md](DESIGN.md).

## Development

```bash
make lint         # cargo fmt --check + clippy -D warnings
make test         # unit + acceptance tests (all features)
make man          # generate doc/levelize.1 from the CLI definition
make help         # the full target list
```

The test suite includes an EBU Tech 3341/3342 compliance subset for the meter,
end-to-end acceptance tests for the leveling invariants (target accuracy,
ceiling safety, idempotency, determinism), golden stats pinned against a
deterministic speech-shaped fixture, and pathological-input coverage (empty,
truncated, NaN-carrying, and non-audio files). MP3, M4B/M4A, and `flac -t`
verification tests skip themselves when `lame`, `ffmpeg`, or `flac` aren't
installed.

## License

[AGPL-3.0-or-later](LICENSE).
