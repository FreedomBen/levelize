# levelize — Design

A modern command-line reimplementation of The Levelator for podcast and spoken-word audio:
one command in, one evenly-leveled, loudness-compliant file out.

## Background

The Levelator (The Conversations Network, 2006) solved one problem extremely well: spoken-word
recordings where different speakers sit at different levels — one mic hot, one guest quiet, a
phone caller buried. It was not a normalizer, compressor, or limiter, though it borrowed from
all three. Because it worked offline on the whole file, it could compute a time-varying gain
that brought every speaker to a consistent level without the pumping and noise-breathing that
real-time AGC produces, then finished with a limiter. You dragged a WAV onto it and it just
worked.

It has been unmaintained for over a decade: 32-bit GUI builds that no longer run on modern
macOS, WAV/AIFF only, no automation story, and it predates the loudness standards (ITU-R
BS.1770, EBU R128) that podcast platforms now specify delivery targets in.

**levelize** keeps the core promise — zero-knob leveling for speech — and rebuilds it as a
modern CLI: standards-based loudness measurement, modern input formats, scriptable in a
pipeline, distributed as a single static binary.

## Goals

- **Zero required knobs.** `levelize episode.wav` produces a leveled file that meets common
  podcast delivery specs. Every flag is optional.
- **Level between and within speakers.** Quiet voices come up, loud voices come down,
  smoothly — without boosting room tone in pauses and without audible pumping.
- **Standards-based.** Loudness measured per ITU-R BS.1770 / EBU R128; targets expressed in
  LUFS, peaks in dBTP (true peak).
- **Offline advantage.** Two-pass design with full lookahead — the reason the original
  sounded better than real-time AGC.
- **Common formats in** (WAV, AIFF, FLAC, MP3, AAC/M4A/M4B, Ogg Vorbis); **WAV and FLAC out**,
  plus MP3 when `lame` is installed and M4B/M4A (AAC, with chapter marks) when `ffmpeg` is.
- **Fast and bounded.** ≥ 50× real-time; memory stays flat regardless of file length.
- **Scriptable.** Meaningful exit codes, logs to stderr, `--json` stats, stdin/stdout
  piping, deterministic output (same input + flags → byte-identical file).
- **Portable.** Single static binary for Linux, macOS, Windows.

## Non-goals (v1)

- Real-time / streaming operation, DAW plugin, or live monitoring.
- Music mastering — the algorithm is tuned for speech.
- Multitrack mixing or per-speaker track processing (process the mixdown).
- Speaker diarization or per-voice targets — levelize treats the mix as one program.
- Built-in lossy encoders — MP3 shells out to `lame`, AAC to `ffmpeg`; no Opus output.
- Noise reduction, de-essing, EQ, or other repair.
- More than 2 channels (clear error message).
- Metadata beyond basic text tags and M4B/M4A chapter marks — no artwork or full ID3
  fidelity.

## CLI

```
levelize [OPTIONS] <INPUT>...
```

```bash
levelize episode.wav                        # → episode.leveled.wav
levelize -o out/ep12.flac raw/ep12.wav      # output format from the extension
levelize --format mp3 episode.wav           # → episode.leveled.mp3 (uses lame)
levelize book.flac -o book.m4b --chapters toc.txt   # chaptered audiobook (uses ffmpeg)
levelize --profile audiobook book.flac      # ACX-friendly preset
levelize --target -19 interview.mp3         # override loudness target
levelize --analyze *.wav                    # measure and report only, no output
levelize --json episode.wav > stats.json    # machine-readable stats
ffmpeg -i raw.mov -f wav - | levelize - -o episode.wav   # stdin works too
```

### Options

| Flag                            | Default                   | Description                                                     |
| ------------------------------- | ------------------------- | --------------------------------------------------------------- |
| `-o, --output <path>`           | `<input>.leveled.<ext>`   | Output file, or `-` for stdout (only valid with a single input) |
| `--format <fmt>`                | from `-o` ext, else `wav` | `wav` \| `flac` \| `mp3` \| `m4b` \| `m4a` (MP3 shells out to `lame`; M4B/M4A to `ffmpeg`) |
| `--profile <name>`              | `podcast`                 | Loudness preset: `podcast` \| `broadcast` \| `audiobook`        |
| `--target <lufs>`               | per profile               | Integrated loudness target; overrides the profile               |
| `--ceiling <dbtp>`              | per profile               | True-peak ceiling; overrides the profile                        |
| `--max-boost <db>`              | 15                        | Cap on upward gain (protects noise floor)                       |
| `--max-cut <db>`                | 15                        | Cap on downward gain                                            |
| `--leveling <mode>`             | `normal`                  | `gentle` \| `normal` \| `aggressive` — how hard levels converge |
| `--bit-depth <depth>`           | match input               | `16` \| `24` \| `32f` for WAV; `16` \| `24` for FLAC            |
| `--mp3-bitrate <kbps>`          | input's, else 128 st / 96 mono | CBR bitrate for MP3 output (a lossy input's measured bitrate wins, snapped to MP3's CBR steps) |
| `--aac-bitrate <kbps>`          | input's, else 96 st / 64 mono  | Bitrate for M4B/M4A output (a lossy input's measured bitrate wins)                             |
| `--chapters <file>`             | input's chapters          | Chapter marks for M4B/M4A: one `TIME Title` line each, `[H:]MM:SS[.sss]` or plain seconds      |
| `--no-chapters`                 | off                       | Don't carry chapter marks from the input into M4B/M4A output                                   |
| `--analyze`                     | off                       | Analysis pass only: print loudness stats, write nothing         |
| `--json`                        | off                       | Emit stats as JSON on stdout                                    |
| `--force`                       | off                       | Allow overwriting an existing output file                       |
| `-q, --quiet` / `-v, --verbose` | —                         | Logging verbosity (stderr)                                      |

### Profiles

`--profile` sets the loudness defaults; an explicit `--target` / `--ceiling` always wins.

| Profile     | Target (stereo) | Target (mono) | Ceiling   | Intended spec  |
| ----------- | --------------- | ------------- | --------- | -------------- |
| `podcast`   | −16 LUFS        | −19 LUFS      | −1.5 dBTP | Apple Podcasts |
| `broadcast` | −23 LUFS        | −23 LUFS      | −1 dBTP   | EBU R128       |
| `audiobook` | −20 LUFS        | −20 LUFS      | −3 dBTP   | ACX            |

ACX specifies RMS (−18 to −23 dB) and sample peak (≤ −3 dB) rather than LUFS; −20 LUFS with
a −3 dBTP ceiling lands typical speech comfortably within those bounds.

### Behavior rules

- Default output name is `episode.leveled.<ext>` next to the input, extension per `--format`.
- An unrecognized `-o` extension with no `--format` is a usage error — levelize never writes
  one format behind another format's file name.
- Never overwrite an existing file without `--force`.
- **Always** refuse `output == input`, even with `--force` — the render pass re-reads the
  input while writing the output.
- Multiple inputs are processed independently, each to its own derived output name.
- `-` as input reads stdin, spilled byte-for-byte to a temp file first (the two passes need
  a rereadable source). Stdin input requires `-o` or `--analyze` — there is no name to derive.
- `-o -` streams output to stdout. Pass 1 fixes the exact sample count, so headers are
  written up front and WAV/FLAC never need to seek (FLAC's STREAMINFO MD5 stays unset on
  pipes, which the spec allows). With `-o -`, `--json` stats move to stderr.
- MP3 output shells out to `lame`; if it isn't in `PATH`, fail early with a clear error.
  `lame` is fed 32-bit float WAV over a pipe, which caps one MP3 render at WAV's 4 GB
  limit — about 3 hours of stereo at 48 kHz (twice that mono). The cap is checked after
  pass 1, before any rendering, with an error that says what to do instead.
- M4B/M4A output shells out to `ffmpeg` (its built-in AAC encoder), with the same
  fail-early check. It is fed raw 32-bit floats — no WAV framing, so no length cap — and
  muxes into a temp file, because MP4 needs a seekable output; the finished file is then
  copied to the real destination, so `-o -` still works. The muxer runs with `faststart`
  (index up front) and `bitexact` (deterministic output). A `.m4b` additionally gets the
  iTunes audiobook atom (`stik` = 2) so players remember the position; `--chapters` marks
  are passed through as an ffmetadata file. Chapters already in the input carry over by
  default — read back through ffmpeg's own ffmetadata dump, which normalizes every flavor
  (MP4 `chpl` atoms, QuickTime chapter tracks, ID3v2 CHAP frames in MP3s) — with explicit
  `--chapters` replacing them and `--no-chapters` dropping them. Chapter problems never
  fail a render: unreadable chapter metadata or marks past a truncated input's real end
  are reported as a warning and the affected marks are left out. A lossy *input*'s
  measured bitrate (packet
  bytes over duration, so tags and cover art don't inflate it) becomes the default output
  bitrate for both MP3 and AAC; lossless inputs fall back to the static defaults.
- Non-finite samples (NaN, ±inf — broken float WAVs have them) are replaced with silence
  at the decode boundary: untreated, one NaN poisons the loudness filters for the rest of
  the file and would pass into the output.
- Progress bar on a TTY (by decoded duration); suppressed when piped or `--quiet`.
- Logs and progress go to stderr; stdout carries only `--analyze` reports, `--json` output,
  or audio with `-o -`.

### Exit codes

| Code | Meaning                                         |
| ---- | ----------------------------------------------- |
| 0    | Success (all inputs processed)                  |
| 1    | Runtime failure (decode error, I/O, overwrite refused) |
| 2    | Usage error (bad flags/arguments)               |

### `--json` output

One JSON object per input, one per line (NDJSON — `jq` reads it directly). Loudness values
are `null` where the measurement is −∞ (digital silence). `--analyze --json` emits only the
`levelize` and `input` fields.

```json
{
  "levelize": "0.1.0",
  "input":    { "path": "ep12.flac", "duration_s": 3722.4, "sample_rate": 48000,
                "channels": 2, "integrated_lufs": -23.4, "lra_lu": 11.2,
                "true_peak_dbtp": -3.1 },
  "output":   { "path": "ep12.leveled.wav", "integrated_lufs": -16.0, "lra_lu": 4.8,
                "true_peak_dbtp": -1.6, "chapters": 0 },
  "gain":     { "median_db": 7.2, "min_db": -1.9, "max_db": 11.0 },
  "settings": { "profile": "podcast", "target_lufs": -16.0, "ceiling_dbtp": -1.5,
                "leveling": "normal", "format": "wav" },
  "warnings": ["gain capped at +15.0 dB; the -16.0 LUFS target was not reached"]
}
```

## Signal pipeline

Two streaming passes over the input. Nothing holds the full file in memory; the only state
kept between passes is the gain curve (one control point per 100 ms — ~36 k points per hour).

```
Pass 1 — analyze (streaming decode)
  input ─▶ decode ─▶ K-weighting ─▶ momentary (400 ms) / short-term (3 s) /
                                    integrated + gating / LRA / true peak
                                          │
                                          ▼
                     program vs. pause classification (R128 gates + noise floor)
                                          ▼
                     raw gain curve: target − short-term, clamped, held in pauses
                                          ▼
                     smoothing: slew-limited in speech, jumps placed in pauses
                                          ▼
                     static trim: predicted integrated loudness → exactly on target

Pass 2 — render (streaming decode)
  input ─▶ decode ─▶ apply gain curve (dB-linear interpolation)
                   ─▶ true-peak limiter (ceiling) ─▶ dither ─▶ encode (WAV | FLAC | MP3 | M4B/M4A)
```

### Pass 1 — analysis and gain planning

1. **Decode** to interleaved f32 at the native sample rate (no resampling).
2. **Measure** per BS.1770: K-weighted momentary (400 ms) and short-term (3 s) loudness on a
   100 ms hop, integrated loudness with the R128 two-stage gate (−70 LUFS absolute, −10 LU
   relative), loudness range (LRA), and 4×-oversampled true peak.
3. **Classify** each hop as *program* (speech) or *pause*: a hop is pause when its
   momentary loudness sits at or below `max(−70 LUFS, min(floor + 6 dB, relative gate))`,
   where the floor is a low percentile (1st) of momentary loudness. This is the guard
   against the classic AGC failure of amplifying room tone between sentences. The
   threshold is anchored to the noise floor rather than to the R128 relative gate because
   the relative gate (integrated − 10 LU) would swallow the quiet half of a two-speaker
   recording with a wide spread — the exact material this tool exists for; the gate
   instead *caps* the threshold (nothing above it is ever called pause, protecting
   against a bad floor estimate), and the −70 LUFS absolute gate is the backstop.
4. **Raw gain curve.** For program hops: `g[i] = target − S[i]` (short-term loudness),
   clamped to `[−max_cut, +max_boost]`. The 3 s short-term window is the control signal
   because it tracks speaker-to-speaker differences while ignoring syllable-level
   dynamics, which we want to keep. The trailing window reads 1.5 s late, so each value
   is attributed to its window *center* — offline, the plan can react on time. Within a
   speech run, hops near the edges reuse the nearest fully-in-run window value, keeping
   pause audio out of the control signal.
5. **Smooth.** Offline lookahead lets us do what real-time AGC can't: within a continuous
   speech run the gain is slew-limited to ~2 dB/s (inaudible drift) by a forward and a
   backward tracking pass, averaged — ramps center on the level change instead of lagging
   it. Larger corrections — e.g. a +10 dB jump at a speaker change — are placed *inside
   pauses*: the gain holds flat into the pause from both sides and steps once at the
   pause midpoint, where the 100 ms between control points is the whole (masked) ramp.
   Pauses too short for a clean jump are ramped across; leading/trailing pauses extend
   the nearest program gain. `--leveling` scales the slew limit, the gain clamps, and the
   shortest pause that takes a jump:

   | Mode         | Slew limit | Clamp scale | Jump pauses |
   | ------------ | ---------- | ----------- | ----------- |
   | `gentle`     | 1 dB/s     | × 0.6       | ≥ 400 ms    |
   | `normal`     | 2 dB/s     | × 1.0       | ≥ 300 ms    |
   | `aggressive` | 4 dB/s     | × 1.0       | ≥ 200 ms    |
6. **Static trim.** K-weighted block powers from step 2 scale as `10^(g/10)` under gain, so
   the post-leveling integrated loudness is computed analytically (re-running the R128 gate
   on adjusted blocks) without touching samples. The trim `target − predicted` is folded
   into the curve so integrated loudness lands on target, not merely near it.

### Pass 2 — render

1. **Decode again** (decoding is cheap; this keeps memory flat and the passes sample-aligned,
   since the same decoder produces the same stream).
2. **Apply gain**, interpolating control points linearly in dB per sample.
3. **True-peak limiter:** ~5 ms lookahead ramp, 4×-oversampled peak detection, ~200 ms
   release, hard ceiling guarantee. With the trim landing loudness on target, the limiter
   only catches transients; its gain reduction costs at most a fraction of an LU.
4. **Dither** (TPDF, 1 LSB) when quantizing to 16-bit; none at 24-bit/32f or on the MP3
   and AAC paths, which are fed float samples.
5. **Encode:** streaming WAV or FLAC writer, or pipe to `lame` for MP3 or `ffmpeg` for
   M4B/M4A — with basic tags copied over (see Metadata).

### Channels, rates, edge cases

- **Stereo is gain-linked:** loudness is measured across channels per BS.1770 and one gain
  curve is applied to both, preserving the stereo image. Default targets come from the
  profile (podcast: −16 stereo / −19 mono); an explicit `--target` always wins.
- Sample rate is preserved end to end; the only oversampling is internal to peak detection.
- **All-pause input** (silence, room tone only): warn, copy through unchanged.
- **Target unreachable** (quiet recording hits `--max-boost`): do what's possible, warn, and
  report the shortfall in stats/JSON.
- Already-leveled input: the curve comes out flat, so the file passes through essentially
  unchanged — idempotency is an explicit acceptance test.

### Metadata

Basic text tags are copied from input to output: title, artist, album/show, date, genre,
comment, and track number. They are read through symphonia's metadata API (ID3v2, Vorbis
comment, RIFF INFO) and written in the output's native form — RIFF INFO for WAV, Vorbis
comments for FLAC, ID3 via `lame`'s tag arguments for MP3, iTunes atoms via `ffmpeg`'s
metadata arguments for M4B/M4A. Chapter marks flow into M4B/M4A output from the input
(any flavor ffmpeg can read) or from a `--chapters` text file, which takes precedence;
artwork and nonstandard tags are not carried over in v1, and no other output format can
hold chapters.

## Technology

**Language: Rust** (decided).

| Option            | Verdict | Notes                                                          |
| ----------------- | ------- | -------------------------------------------------------------- |
| Rust              | ✅ pick | Mature pure-Rust audio stack, fast, single static binary, easy cross-compile |
| Go                | ❌      | Weak audio ecosystem; no maintained BS.1770 implementation      |
| Python            | ❌      | Easy prototype (pyloudnorm), but heavy to distribute as a CLI   |
| C/C++             | ❌      | Capable but more effort for memory safety, tooling, packaging   |
| ffmpeg `loudnorm` wrapper | ❌ | Not the same algorithm: static/EBU-linear normalization with a dynamic mode prone to artifacts; no pause-aware speaker leveling — and it's the core of this project |

### Dependencies

| Crate        | Role                                                        |
| ------------ | ----------------------------------------------------------- |
| `symphonia`  | Decoding: WAV, AIFF, FLAC, MP3, AAC, Ogg Vorbis (pure Rust) |
| `ebur128`    | BS.1770 / R128 loudness + true peak (pure-Rust libebur128 port) |
| `flacenc`    | FLAC encoding (pure Rust)                                   |
| `clap`       | CLI parsing (derive), usage errors → exit 2                 |
| `indicatif`  | Progress bar                                                |
| `serde_json` | `--json` output                                             |
| `tempfile`   | Stdin spilled to a rereadable temp file; test fixtures      |
| `anyhow`     | Error handling                                              |

No linked system libraries; the only externals are optional binaries run as child
processes — `lame` for MP3 output and `ffmpeg` for M4B/M4A. The gain planner, limiter, dither, and
WAV writer are hand-rolled — the DSP because it is small, well-understood, and the heart of
the product; the WAV writer because pass 1 fixes the sample count, which lets it write
headers up front (pipe-friendly) and RIFF INFO tags, neither of which off-the-shelf writers
cover.

## Architecture

Library + thin binary, so the core is testable and reusable:

```
src/
  main.rs        CLI entry: parse args, per-file orchestration, exit codes
  lib.rs         Public API: analyze(), level() with a Settings struct
  decode.rs      Symphonia wrapper → iterator of f32 frame chunks; stdin → temp spill
  meter.rs       BS.1770 measurements, block energies, noise-floor estimate
  plan.rs        Classification, raw curve, smoothing, static trim → GainCurve
  render.rs      Gain application, true-peak limiter, dither
  encode.rs      WAV/FLAC writers, lame/ffmpeg pipes for MP3 and M4B/M4A, tags, chapters
  stats.rs       Human + JSON report types
```

Data flow matches the pipeline diagram: `decode → meter → plan` (pass 1), then
`decode → render → encode` (pass 2), with `GainCurve` (control points + static trim) as the
only artifact passed between them.

**Determinism:** fixed-order f32 math, no threading inside the DSP path in v1. Parallelism,
if ever needed, goes across files, not within one.

## Performance targets

| Metric       | Target                                               | Measured (M6)        |
| ------------ | ---------------------------------------------------- | -------------------- |
| Throughput   | ≥ 50× real-time (1 h stereo 44.1 kHz in ≤ ~72 s) on a typical laptop | 109× (33 s for 1 h)  |
| Memory       | < 200 MB RSS regardless of input length              | 6.8 MB max RSS       |
| Startup      | < 50 ms to first byte of work                        | < 10 ms              |

Measured on the M6 hardening pass: release build, single thread, 1 h stereo 44.1 kHz WAV
in and out, Ryzen 9 5950X, Fedora 43 (`/usr/bin/time -v`).

Decoding twice instead of caching PCM is the deliberate trade: a 3-hour stereo file is
~4 GB as f32, while re-decoding costs seconds and keeps memory flat.

## Testing

- **Unit (DSP):** synthetic signals generated in-test (no binary fixtures in the repo):
  sines at known LUFS, step-level "two speakers", speech-shaped noise bursts with gaps.
- **Meter compliance:** the synthesizable subset of EBU Tech 3341/3342 test cases against
  our measurement path (also validates our use of `ebur128`).
- **Acceptance invariants** on rendered output:
  - `|integrated − target| ≤ 0.5 LU` (when reachable within gain caps)
  - true peak ≤ ceiling, always
  - pause segments gain-held: noise floor rises no more than the adjacent program gain
  - idempotency: second run changes integrated loudness ≤ 0.5 LU
  - determinism: identical bytes across repeated runs
- **Golden end-to-end:** a deterministic speech-shaped fixture (seeded noise through a
  syllable envelope: quiet guest, loud host, room-tone pauses — synthesized in-test, so the
  repo stays free of binary blobs and the test hermetic), leveled with default settings and
  asserted against stored *stats* (not waveforms, and with ±0.1 dB slack for libm
  differences) so only an algorithm change can move them.
- **Edge cases:** empty and sub-hop inputs copy through cleanly; truncated files level
  what's there; NaN/±inf samples are silenced, never propagated; >2 channels and non-audio
  files fail cleanly without stopping a batch; 8 kHz and 192 kHz still land on target.
- **Output formats:** FLAC output decodes back bit-identical to the equivalent WAV; MP3,
  M4B/M4A (tags, chapters, the audiobook atom, bitrate matching), and stdin/stdout paths
  covered by round-trip integration tests (CI installs `lame` and `ffmpeg` on the Linux
  runner; the tests skip themselves where the encoders are missing).
- CI: `cargo test` + clippy + fmt on a Linux/macOS/Windows matrix.

## Distribution

- `cargo install levelize`; prebuilt release binaries from CI on `v*` tags (Linux musl
  static x86_64/aarch64, macOS universal, Windows), checksummed and attached to the GitHub
  release, plus `.deb`/`.rpm` packages of the static Linux binaries (`cargo deb` /
  `cargo generate-rpm`, metadata in Cargo.toml; `make deb` / `make rpm` locally). The
  release checklist lives in RELEASING.md; crates.io publishing is manual.
- Man page generated from clap via `clap_mangen` behind the `mangen` feature (`make man`
  → `doc/levelize.1`, shipped in the release archives); `--help` is the primary doc
  surface. `make install` places binary and man page under `PREFIX` (default
  `/usr/local`).
- Semver; `0.x` until the algorithm tuning settles. Changes tracked in CHANGELOG.md.
- Crate name `levelize` confirmed available on crates.io (Oct 2026).

## License

AGPLv3 (SPDX: `AGPL-3.0-or-later`). All crates in the dependency table are MIT/Apache-2.0
dual-licensed, which is compatible with AGPL distribution. `lame` (LGPL) and `ffmpeg`
(LGPL/GPL) are invoked as separate processes, never linked, so they impose nothing on
levelize's licensing. AGPL crates publish to crates.io without issue.

## Milestones

- [x] **M0 — Scaffold:** git repo housekeeping, AGPLv3 LICENSE file + Cargo.toml metadata,
      cargo project (lib + bin), clap skeleton, `--version`, CI with fmt/clippy/test
- [x] **M1 — Meter:** WAV decode → `--analyze` reporting integrated/LRA/true peak;
      Tech 3341/3342 tests pass
- [x] **M2 — Static pipeline:** fixed-gain normalize → limiter → dither → WAV out;
      acceptance invariants green (this is already a useful loudness normalizer)
- [x] **M3 — Adaptive leveling:** classification, gain curve, smoothing, static trim —
      the Levelator core; tune with listening tests
- [x] **M4 — Formats & channels:** symphonia inputs (FLAC/MP3/AAC/Ogg/AIFF), mono/stereo
      auto-targets, multi-file inputs, FLAC output, MP3 output via `lame`, tag copying
- [x] **M5 — UX:** progress, `--json`, warnings, exit codes, `--profile` presets,
      stdin/stdout mode, polished `--help`, README
- [x] **M6 — Hardening:** golden tests, edge cases (silence, unreachable target,
      pathological inputs), performance pass against targets
- [x] **M7 — Release:** binaries, `cargo install`, man page, v0.1 announcement notes
      (release workflow, Makefile, CHANGELOG, RELEASING checklist; the tag/publish acts
      themselves run per RELEASING.md)

## References

- ITU-R BS.1770 — Algorithms to measure audio programme loudness and true-peak level:
  <https://www.itu.int/rec/R-REC-BS.1770>
- EBU R128 — Loudness normalisation: <https://tech.ebu.ch/publications/r128>
- EBU Tech 3341 / 3342 — Loudness metering test material:
  <https://tech.ebu.ch/publications/tech3341>, <https://tech.ebu.ch/publications/tech3342>
- Apple Podcasts audio requirements (−16 LUFS stereo / −19 mono, ≤ −1 dBTP):
  <https://podcasters.apple.com/support/893-audio-requirements>
- ACX audio submission requirements (audiobook profile):
  <https://help.acx.com/s/article/what-are-the-acx-audio-submission-requirements>
- The Levelator (archived): <http://www.conversationsnetwork.org/levelator>
