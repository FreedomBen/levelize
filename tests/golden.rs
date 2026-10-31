//! Golden end-to-end stats (DESIGN.md "Testing"): a deterministic
//! speech-shaped fixture, leveled with default settings, must keep producing
//! the same numbers. The fixture is synthesized here — hermetic and free of
//! binary blobs — and the assertions pin *stats*, not waveforms, so only an
//! algorithm change can move them.
//!
//! When a deliberate retune moves the numbers, update the `GOLDEN` table from
//! the values the failing assertions print.

mod common;

use std::f64::consts::PI;

use common::write_wav_pcm16;
use levelize::{Output, Settings, level};

const RATE: u32 = 44_100;

/// xorshift64* with a fixed seed: the fixture is identical on every run.
struct Rng(u64);

impl Rng {
    fn next_unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let value = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (value >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// One mono speech-shaped burst: lowpassed noise under a ~3.3 Hz syllable
/// envelope, peaking around `level_dbfs`.
fn speaker(rng: &mut Rng, seconds: f64, level_dbfs: f64) -> Vec<f32> {
    let amplitude = 10f64.powf(level_dbfs / 20.0);
    let frames = (seconds * f64::from(RATE)) as usize;
    let pole = (-2.0 * PI * 900.0 / f64::from(RATE)).exp();
    let mut state = 0.0f64;
    (0..frames)
        .map(|i| {
            let t = i as f64 / f64::from(RATE);
            let syllable = (2.0 * PI * 3.3 * t).sin().max(0.0);
            state = pole * state + (1.0 - pole) * rng.next_unit();
            (amplitude * syllable * state * 8.0) as f32
        })
        .collect()
}

/// Mono room tone at −66 dBFS: quiet, but not digital silence, so the
/// planner's noise-floor estimate has something to anchor to.
fn room_tone(rng: &mut Rng, seconds: f64) -> Vec<f32> {
    let amplitude = 10f64.powf(-66.0 / 20.0);
    let frames = (seconds * f64::from(RATE)) as usize;
    (0..frames)
        .map(|_| (amplitude * rng.next_unit()) as f32)
        .collect()
}

/// A quiet guest and a loud host taking turns, with room tone between.
fn fixture() -> Vec<f32> {
    let mut rng = Rng(0x5EED_CAFE_F00D_D00D);
    let mut mono = Vec::new();
    mono.extend(room_tone(&mut rng, 0.8));
    mono.extend(speaker(&mut rng, 6.0, -26.0));
    mono.extend(room_tone(&mut rng, 1.0));
    mono.extend(speaker(&mut rng, 6.0, -18.0));
    mono.extend(room_tone(&mut rng, 0.6));
    mono.extend(speaker(&mut rng, 5.0, -26.0));
    mono.extend(room_tone(&mut rng, 0.8));
    mono.iter().flat_map(|&s| [s, s]).collect()
}

struct Golden {
    name: &'static str,
    value: fn(&levelize::LevelReport) -> f64,
    golden: f64,
}

/// Captured from the first green run (levelize 0.1.0, 2026-10-31).
const GOLDEN: &[Golden] = &[
    Golden {
        name: "input integrated LUFS",
        value: |r| r.input.loudness.integrated_lufs,
        golden: -22.224,
    },
    Golden {
        name: "input LRA LU",
        value: |r| r.input.loudness.lra_lu,
        golden: 8.497,
    },
    Golden {
        name: "input true peak dBTP",
        value: |r| r.input.loudness.true_peak_dbtp,
        golden: -5.039,
    },
    Golden {
        name: "output integrated LUFS",
        value: |r| r.output_loudness.integrated_lufs,
        golden: -15.995,
    },
    Golden {
        name: "output LRA LU",
        value: |r| r.output_loudness.lra_lu,
        golden: 1.253,
    },
    Golden {
        name: "output true peak dBTP",
        value: |r| r.output_loudness.true_peak_dbtp,
        golden: -1.795,
    },
    Golden {
        name: "gain median dB",
        value: |r| r.gain_db,
        golden: 10.688,
    },
    Golden {
        name: "gain min dB",
        value: |r| r.gain_min_db,
        golden: 2.621,
    },
    Golden {
        name: "gain max dB",
        value: |r| r.gain_max_db,
        golden: 11.135,
    },
];

#[test]
fn default_leveling_stats_match_the_goldens() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("golden.wav");
    write_wav_pcm16(&input, RATE, 2, &fixture());
    let output = dir.path().join("golden.leveled.wav");
    let report = level(&input, Output::File(&output), &Settings::default()).unwrap();

    assert!(!report.silent_input);
    assert!(!report.gain_clamped);
    let drifted: Vec<String> = GOLDEN
        .iter()
        .filter(|entry| ((entry.value)(&report) - entry.golden).abs() > 0.1)
        .map(|entry| {
            format!(
                "  {}: measured {:.3}, golden {:.3}",
                entry.name,
                (entry.value)(&report),
                entry.golden
            )
        })
        .collect();
    assert!(
        drifted.is_empty(),
        "stats drifted from the goldens — a deliberate algorithm change means \
         updating the GOLDEN table from these measurements; anything else is \
         a regression:\n{}",
        drifted.join("\n")
    );
}
