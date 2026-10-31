# levelize 0.1 — announcement draft

Short post for r/rust, Hacker News, Mastodon, or a podcasting forum. Trim to
taste; the first paragraph stands alone for length-limited places.

---

**levelize: adaptive loudness leveling for spoken-word audio (a modern
Levelator, in Rust)**

In 2006, The Levelator did one thing startlingly well: you dropped a podcast
WAV on it, and every voice came out at the same comfortable level — the quiet
guest brought up, the loud host reined in, pauses and room tone left alone.
It's abandonware now, and the loudness world has since standardized on
BS.1770/EBU R128 integrated loudness and true-peak ceilings.

levelize is that tool rebuilt for today: a single fast CLI that levels speech
*between and within speakers*, then lands the result exactly on a standard
loudness target (−16 LUFS stereo podcast by default, with broadcast and
audiobook presets) under a true-peak-safe ceiling. A static normalizer like
`ffmpeg loudnorm` or ReplayGain moves the whole file by one gain; levelize
plans a gain curve over time, so a guest recorded 12 dB quieter than the host
comes out at the same loudness — without pumping room tone up between
sentences, the classic AGC failure.

```console
$ levelize episode.wav
episode.wav → episode.leveled.wav
  integrated   -15.1 → -16.0 LUFS   (gain +14.1 dB)
  gain range   -0.9 … +14.5 dB
  true peak    -15.0 → -15.5 dBTP
```

Under the hood: two streaming passes (nothing holds the file in memory),
BS.1770 short-term loudness driving a slew-limited gain curve with larger
corrections jumped at pause midpoints where a step can't be heard, an
analytic trim that lands the R128-gated integrated loudness on target, and a
4×-oversampled lookahead limiter for the ceiling. Reads WAV/AIFF/FLAC/MP3/
AAC/Ogg; writes WAV/FLAC/MP3; copies tags; `--json` stats and stdin/stdout
filter mode for scripting; byte-deterministic output. Roughly 100× realtime
on one core, flat memory.

- Install: `cargo install levelize`, or prebuilt static binaries for
  Linux/macOS/Windows on the releases page.
- Source (AGPL-3.0-or-later): https://github.com/FreedomBen/levelize
- Design doc with the measurement math and acceptance invariants:
  DESIGN.md in the repo.

It's 0.1: the algorithm's tuning may still move. I'd love ears on it —
especially recordings where it misbehaves.
