//! Pass-1 gain planning: program/pause classification, the adaptive gain
//! curve, smoothing with jumps placed in pauses, and the analytic static trim
//! (DESIGN.md "Pass 1 — analysis and gain planning").
//!
//! The planner works on 100 ms hops. After each hop the analyzer records the
//! BS.1770 momentary (400 ms) and short-term (3 s) loudness; those two series
//! drive everything downstream:
//!
//!   - momentary → classification (program vs. pause) and the static trim's
//!     gating blocks — at a 100 ms hop they are exactly the R128 gating grid
//!   - short-term → the gain control signal, attributed to its window center
//!     so the plan reacts on time instead of 1.5 s late
//!
//! Classification is anchored to the noise floor (a low percentile of
//! momentary loudness plus a margin), capped by the R128 relative gate so a
//! bad floor estimate can never swallow real program, with the −70 LUFS
//! absolute gate as the backstop. The relative gate alone would misclassify
//! the quiet half of a two-speaker recording with a wide spread — the exact
//! case this tool exists for.
//!
//! `--leveling` scales the slew limit, the gain clamps, and the shortest
//! pause that takes a gain jump:
//!
//! | mode       | slew   | clamp scale | jump pauses |
//! | ---------- | ------ | ----------- | ----------- |
//! | gentle     | 1 dB/s | × 0.6       | ≥ 400 ms    |
//! | normal     | 2 dB/s | × 1.0       | ≥ 300 ms    |
//! | aggressive | 4 dB/s | × 1.0       | ≥ 200 ms    |

use anyhow::Result;

use crate::meter::{Loudness, Meter};
use crate::{Leveling, Settings};

/// Hop length: the planner's control-point spacing.
pub const HOP_SECS: f64 = 0.1;
/// Short-term window length in hops (3 s).
const SHORT_TERM_HOPS: usize = 30;
/// Hops between a short-term query and its window center.
const CENTER_SHIFT: usize = 15;
/// R128 absolute gate.
const ABS_GATE_LUFS: f64 = -70.0;
/// Noise-floor estimate: low percentile of momentary loudness...
const FLOOR_PERCENTILE: f64 = 0.01;
/// ...and hops within this margin of the floor are pauses.
const FLOOR_MARGIN_DB: f64 = 6.0;
/// Stand-in for `-inf` momentary values in the floor percentile.
const SILENT_HOP_LUFS: f64 = -120.0;
/// Static-trim iteration: stop when within this of the target, or after
/// this many rounds (the gain clamps can make the target unreachable).
const TRIM_EPSILON_DB: f64 = 0.05;
const TRIM_ROUNDS: usize = 8;

/// Everything pass 1 learned about one input.
pub struct Analysis {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub loudness: Loudness,
    /// Momentary loudness after each 100 ms hop (LUFS).
    pub momentary: Vec<f64>,
    /// Short-term loudness after each 100 ms hop (LUFS).
    pub short_term: Vec<f64>,
}

/// Streaming pass-1 analyzer: a [`Meter`] plus per-hop measurements.
pub struct Analyzer {
    meter: Meter,
    channels: usize,
    hop_frames: usize,
    in_hop: usize,
    frames: u64,
    sample_rate: u32,
    momentary: Vec<f64>,
    short_term: Vec<f64>,
}

impl Analyzer {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self> {
        Ok(Analyzer {
            meter: Meter::new(sample_rate, channels)?,
            channels: usize::from(channels),
            hop_frames: hop_frames(sample_rate),
            in_hop: 0,
            frames: 0,
            sample_rate,
            momentary: Vec::new(),
            short_term: Vec::new(),
        })
    }

    /// Feed one interleaved chunk of whole frames.
    pub fn feed(&mut self, interleaved: &[f32]) -> Result<()> {
        let mut rest = interleaved;
        while !rest.is_empty() {
            let frames_left = self.hop_frames - self.in_hop;
            let take_frames = frames_left.min(rest.len() / self.channels);
            let take_samples = take_frames * self.channels;
            self.meter.feed(&rest[..take_samples])?;
            rest = &rest[take_samples..];
            self.in_hop += take_frames;
            self.frames += take_frames as u64;
            if self.in_hop == self.hop_frames {
                self.momentary.push(self.meter.momentary()?);
                self.short_term.push(self.meter.short_term()?);
                self.in_hop = 0;
            }
        }
        Ok(())
    }

    /// Final measurements; a trailing partial hop contributes to the overall
    /// loudness but gets no control point of its own.
    pub fn finish(self) -> Result<Analysis> {
        let channels = self.channels as u16;
        Ok(Analysis {
            sample_rate: self.sample_rate,
            channels,
            frames: self.frames,
            loudness: self.meter.finish()?,
            momentary: self.momentary,
            short_term: self.short_term,
        })
    }
}

/// Frames per 100 ms hop.
fn hop_frames(sample_rate: u32) -> usize {
    ((f64::from(sample_rate) * HOP_SECS).round() as usize).max(1)
}

/// The planned gain curve: one control point per hop, applied at hop centers
/// with dB-linear interpolation in between and constant extension outside.
pub struct GainCurve {
    points_db: Vec<f64>,
    hop_frames: f64,
    /// Predicted post-leveling integrated loudness (R128-gated), LUFS.
    pub predicted_lufs: f64,
    /// No program material was found; the curve is flat 0 dB.
    pub all_pause: bool,
    /// The gain clamps kept the predicted loudness off the target.
    pub clamped: bool,
    pub median_db: f64,
    pub min_db: f64,
    pub max_db: f64,
}

/// Per-mode shape: slew limit, clamp scaling, shortest jumpable pause.
struct Shape {
    slew_db_per_s: f64,
    clamp_scale: f64,
    jump_hops: usize,
}

fn shape(leveling: Leveling) -> Shape {
    match leveling {
        Leveling::Gentle => Shape {
            slew_db_per_s: 1.0,
            clamp_scale: 0.6,
            jump_hops: 4,
        },
        Leveling::Normal => Shape {
            slew_db_per_s: 2.0,
            clamp_scale: 1.0,
            jump_hops: 3,
        },
        Leveling::Aggressive => Shape {
            slew_db_per_s: 4.0,
            clamp_scale: 1.0,
            jump_hops: 2,
        },
    }
}

/// Plan the gain curve for one analyzed input.
pub fn plan(analysis: &Analysis, settings: &Settings, target_lufs: f64) -> GainCurve {
    let shape = shape(settings.leveling);
    let boost = settings.max_boost_db * shape.clamp_scale;
    let cut = settings.max_cut_db * shape.clamp_scale;
    let hops = analysis.momentary.len();
    let hop_frames = hop_frames(analysis.sample_rate) as f64;

    let program = classify(&analysis.momentary);
    let runs = program_runs(&program);
    if runs.is_empty() {
        return GainCurve {
            points_db: vec![0.0; hops.max(1)],
            hop_frames,
            predicted_lufs: analysis.loudness.integrated_lufs,
            all_pause: true,
            clamped: false,
            median_db: 0.0,
            min_db: 0.0,
            max_db: 0.0,
        };
    }

    // Raw curve per program run: target − centered short-term, clamped, then
    // slew-limited (forward/backward tracking passes, averaged, so ramps
    // center on the level change instead of lagging it).
    let mut points = vec![f64::NAN; hops];
    let max_step = shape.slew_db_per_s * HOP_SECS;
    for &(a, b) in &runs {
        let first_full = (a + SHORT_TERM_HOPS - 1).min(b);
        let raw: Vec<f64> = (a..=b)
            .map(|i| {
                let ctrl = analysis.short_term[(i + CENTER_SHIFT).clamp(first_full, b)];
                (target_lufs - ctrl).clamp(-cut, boost)
            })
            .collect();
        let forward = track(raw.iter().copied(), max_step, raw.len());
        let backward = {
            let mut tracked = track(raw.iter().rev().copied(), max_step, raw.len());
            tracked.reverse();
            tracked
        };
        for (offset, i) in (a..=b).enumerate() {
            points[i] = (forward[offset] + backward[offset]) / 2.0;
        }
    }
    fill_pauses(&mut points, &runs, shape.jump_hops);

    // Static trim: predict the gated loudness analytically from the gained
    // momentary blocks and fold `target − predicted` into the whole curve,
    // iterating because the gain shifts which blocks the gate keeps.
    let clamp_all = |trim: f64| -> Vec<f64> {
        points
            .iter()
            .map(|p| (p + trim).clamp(-cut, boost))
            .collect()
    };
    let mut trim = 0.0;
    let mut effective = clamp_all(trim);
    let mut predicted = predict_integrated(&analysis.momentary, &effective);
    for _ in 0..TRIM_ROUNDS {
        if !predicted.is_finite() {
            break;
        }
        let delta = target_lufs - predicted;
        if delta.abs() <= TRIM_EPSILON_DB {
            break;
        }
        trim += delta;
        let next = clamp_all(trim);
        if next == effective {
            break;
        }
        effective = next;
        predicted = predict_integrated(&analysis.momentary, &effective);
    }

    let mut sorted = effective.clone();
    sorted.sort_by(f64::total_cmp);
    let median_db = if sorted.len() % 2 == 1 {
        sorted[sorted.len() / 2]
    } else {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    };
    let clamped = predicted.is_finite() && (target_lufs - predicted).abs() > 0.25;
    GainCurve {
        min_db: sorted[0],
        max_db: sorted[sorted.len() - 1],
        median_db,
        points_db: effective,
        hop_frames,
        predicted_lufs: predicted,
        all_pause: false,
        clamped,
    }
}

/// Program/pause flag per hop. Pause = momentary at or under
/// `max(−70, min(floor + margin, relative gate))`.
fn classify(momentary: &[f64]) -> Vec<bool> {
    let mut sorted: Vec<f64> = momentary
        .iter()
        .map(|&l| if l.is_finite() { l } else { SILENT_HOP_LUFS })
        .collect();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() {
        return Vec::new();
    }
    let index = ((sorted.len() - 1) as f64 * FLOOR_PERCENTILE).round() as usize;
    let floor = sorted[index];
    let (_, relative_gate) = gated_integrated(momentary.iter().copied());
    let threshold = ABS_GATE_LUFS.max((floor + FLOOR_MARGIN_DB).min(relative_gate));
    momentary.iter().map(|&l| l > threshold).collect()
}

/// Maximal runs of consecutive program hops, inclusive.
fn program_runs(program: &[bool]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, &on) in program.iter().enumerate() {
        match (on, start) {
            (true, None) => start = Some(i),
            (false, Some(a)) => {
                runs.push((a, i - 1));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(a) = start {
        runs.push((a, program.len() - 1));
    }
    runs
}

/// One-pole-free rate limiter: each output chases its input within
/// `max_step` of the previous output.
fn track(raw: impl Iterator<Item = f64>, max_step: f64, len: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(len);
    let mut prev = f64::NAN;
    for value in raw {
        prev = if prev.is_nan() {
            value
        } else {
            value.clamp(prev - max_step, prev + max_step)
        };
        out.push(prev);
    }
    out
}

/// Fill the pause gaps between program runs: hold across short pauses with a
/// linear ramp, place a single jump at the midpoint of pauses at least
/// `jump_hops` long, and extend the first/last program gain to the edges.
fn fill_pauses(points: &mut [f64], runs: &[(usize, usize)], jump_hops: usize) {
    let first = runs[0];
    let last = runs[runs.len() - 1];
    for i in 0..first.0 {
        points[i] = points[first.0];
    }
    for i in (last.1 + 1)..points.len() {
        points[i] = points[last.1];
    }
    for pair in runs.windows(2) {
        let (gap_start, gap_end) = (pair[0].1 + 1, pair[1].0 - 1);
        let (left, right) = (points[pair[0].1], points[pair[1].0]);
        let len = gap_end - gap_start + 1;
        for (offset, i) in (gap_start..=gap_end).enumerate() {
            points[i] = if len >= jump_hops {
                // Jump at the pause midpoint; the 100 ms between control
                // points is the whole (masked) ramp.
                if offset < len / 2 { left } else { right }
            } else {
                // Too short for a clean jump: ramp across the gap.
                left + (right - left) * (offset + 1) as f64 / (len + 1) as f64
            };
        }
    }
}

/// R128 two-stage gate over 400 ms block loudness values: returns
/// `(integrated, relative_threshold)` in LUFS, `-inf` when nothing survives.
fn gated_integrated(blocks: impl Iterator<Item = f64> + Clone) -> (f64, f64) {
    let absolute = mean_energy(blocks.clone().filter(|&l| l > ABS_GATE_LUFS));
    let Some(absolute) = absolute else {
        return (f64::NEG_INFINITY, f64::NEG_INFINITY);
    };
    let relative_threshold = energy_to_lufs(absolute) - 10.0;
    let gated = mean_energy(blocks.filter(|&l| l > relative_threshold));
    let integrated = gated.map_or(f64::NEG_INFINITY, energy_to_lufs);
    (integrated, relative_threshold)
}

/// Predicted post-gain integrated loudness: each momentary block scales by
/// the mean curve gain over the hops it spans, then the gate re-runs.
fn predict_integrated(momentary: &[f64], points_db: &[f64]) -> f64 {
    let shifted = momentary.iter().enumerate().map(|(i, &block)| {
        let span = &points_db[i.saturating_sub(3)..=i];
        let gain: f64 = span.iter().sum::<f64>() / span.len() as f64;
        block + gain
    });
    gated_integrated(shifted).0
}

fn lufs_to_energy(lufs: f64) -> f64 {
    10f64.powf((lufs + 0.691) / 10.0)
}

fn energy_to_lufs(energy: f64) -> f64 {
    -0.691 + 10.0 * energy.log10()
}

fn mean_energy(blocks: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = blocks.fold((0.0, 0u64), |(sum, count), lufs| {
        (sum + lufs_to_energy(lufs), count + 1)
    });
    (count > 0).then(|| sum / count as f64)
}

impl GainCurve {
    /// Control points in dB, one per hop.
    pub fn points_db(&self) -> &[f64] {
        &self.points_db
    }

    pub fn sampler(&self) -> Sampler<'_> {
        Sampler {
            points: &self.points_db,
            hop_frames: self.hop_frames,
            frame: 0,
        }
    }
}

/// Per-frame linear gain: call [`Sampler::next_gain`] once per frame, in
/// order, starting at the first frame of the input.
pub struct Sampler<'a> {
    points: &'a [f64],
    hop_frames: f64,
    frame: u64,
}

impl Sampler<'_> {
    pub fn next_gain(&mut self) -> f64 {
        let position = (self.frame as f64 + 0.5) / self.hop_frames - 0.5;
        self.frame += 1;
        let last = self.points.len() - 1;
        let db = if position <= 0.0 {
            self.points[0]
        } else if position >= last as f64 {
            self.points[last]
        } else {
            let k = position as usize;
            let fraction = position - k as f64;
            self.points[k] + (self.points[k + 1] - self.points[k]) * fraction
        };
        10f64.powf(db / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Settings;

    fn analysis(momentary: Vec<f64>, short_term: Vec<f64>) -> Analysis {
        let frames = momentary.len() as u64 * 4800;
        Analysis {
            sample_rate: 48_000,
            channels: 2,
            frames,
            loudness: Loudness {
                integrated_lufs: -23.0,
                lra_lu: 0.0,
                true_peak_dbtp: -10.0,
            },
            momentary,
            short_term,
        }
    }

    #[test]
    fn hop_measurements_line_up_with_the_meter() {
        let mut analyzer = Analyzer::new(48_000, 1).unwrap();
        let amplitude = 10f32.powf(-23.0 / 20.0);
        let samples: Vec<f32> = (0..96_000)
            .map(|n| amplitude * (2.0 * std::f32::consts::PI * 997.0 * n as f32 / 48_000.0).sin())
            .collect();
        for chunk in samples.chunks(1111) {
            analyzer.feed(chunk).unwrap();
        }
        let analysis = analyzer.finish().unwrap();
        assert_eq!(analysis.frames, 96_000);
        assert_eq!(analysis.momentary.len(), 20);
        assert_eq!(analysis.short_term.len(), 20);
        // Mono sine at −23 dBFS reads −26 LUFS once the window is primed.
        let last = analysis.momentary[19];
        assert!((last + 26.0).abs() < 0.2, "{last}");
    }

    #[test]
    fn jump_lands_inside_the_pause() {
        // 4 s at one level, a 1 s pause, 4 s at another: the gain must hold
        // flat through each run and step once, mid-pause.
        let mut momentary = vec![-25.0; 40];
        momentary.extend(vec![-60.0; 10]);
        momentary.extend(vec![-15.0; 40]);
        let mut short_term = vec![-25.0; 40];
        short_term.extend(vec![-60.0; 10]);
        short_term.extend(vec![-15.0; 40]);
        let curve = plan(
            &analysis(momentary, short_term),
            &Settings::default(),
            -16.0,
        );

        let p = curve.points_db();
        // Flat within each program run...
        for window in p[0..40].windows(2).chain(p[50..90].windows(2)) {
            assert!((window[1] - window[0]).abs() < 1e-9);
        }
        // ...held flat into the pause from both sides...
        assert!((p[44] - p[39]).abs() < 1e-9);
        assert!((p[45] - p[50]).abs() < 1e-9);
        // ...with the whole +9 → −1 step at the pause midpoint.
        let step = p[45] - p[44];
        assert!((step - (-10.0)).abs() < 1e-6, "{step}");
    }

    #[test]
    fn slew_is_limited_within_program() {
        // A hard 10 dB step in short-term loudness mid-run: the curve must
        // move at ≤ 2 dB/s (0.2 dB per hop) and center the ramp on the step.
        // (The step sits at hop 150 in the series; the planner attributes it
        // to hop 135 after centering the short-term window.)
        let momentary = vec![-25.0; 300];
        let mut short_term = vec![-30.0; 150];
        short_term.extend(vec![-20.0; 150]);
        let curve = plan(
            &analysis(momentary, short_term),
            &Settings::default(),
            -16.0,
        );

        let p = curve.points_db();
        for window in p.windows(2) {
            assert!((window[1] - window[0]).abs() <= 0.2 + 1e-9);
        }
        // Ends settle 10 dB apart; the ramp centers on the (shifted) step.
        assert!((p[10] - p[290] - 10.0).abs() < 1e-6, "{} {}", p[10], p[290]);
        let mid = (p[10] + p[290]) / 2.0;
        assert!((p[134] - mid).abs() < 0.5, "{} {mid}", p[134]);
    }

    #[test]
    fn trim_lands_predicted_loudness_on_target() {
        let momentary: Vec<f64> = (0..200)
            .map(|i| if i % 2 == 0 { -20.0 } else { -24.0 })
            .collect();
        let short_term = vec![-22.0; 200];
        let curve = plan(
            &analysis(momentary, short_term),
            &Settings::default(),
            -16.0,
        );
        assert!(
            (curve.predicted_lufs + 16.0).abs() <= TRIM_EPSILON_DB,
            "{}",
            curve.predicted_lufs
        );
        assert!(!curve.clamped);
        assert!(!curve.all_pause);
    }

    #[test]
    fn clamped_curves_report_the_shortfall() {
        let curve = plan(
            &analysis(vec![-45.0; 100], vec![-45.0; 100]),
            &Settings::default(),
            -16.0,
        );
        assert!(curve.clamped);
        assert_eq!(curve.max_db, 15.0);
        assert_eq!(curve.min_db, 15.0);
        assert!(
            (curve.predicted_lufs + 30.0).abs() < 0.1,
            "{}",
            curve.predicted_lufs
        );
    }

    #[test]
    fn gentle_scales_clamps_and_slew() {
        let settings = Settings {
            leveling: Leveling::Gentle,
            ..Settings::default()
        };
        let curve = plan(
            &analysis(vec![-45.0; 100], vec![-45.0; 100]),
            &settings,
            -16.0,
        );
        // Clamp scale 0.6 × 15 dB.
        assert_eq!(curve.max_db, 9.0);

        let momentary = vec![-25.0; 100];
        let mut short_term = vec![-30.0; 50];
        short_term.extend(vec![-20.0; 50]);
        let curve = plan(&analysis(momentary, short_term), &settings, -16.0);
        for window in curve.points_db().windows(2) {
            assert!((window[1] - window[0]).abs() <= 0.1 + 1e-9);
        }
    }

    #[test]
    fn all_pause_input_gets_a_flat_curve() {
        let curve = plan(
            &analysis(vec![-80.0; 50], vec![-80.0; 50]),
            &Settings::default(),
            -16.0,
        );
        assert!(curve.all_pause);
        assert!(!curve.clamped);
        assert!(curve.points_db().iter().all(|&p| p == 0.0));
        assert_eq!(curve.median_db, 0.0);
    }

    #[test]
    fn sampler_interpolates_between_hop_centers() {
        let curve = GainCurve {
            points_db: vec![0.0, 6.0],
            hop_frames: 10.0,
            predicted_lufs: -16.0,
            all_pause: false,
            clamped: false,
            median_db: 3.0,
            min_db: 0.0,
            max_db: 6.0,
        };
        let mut sampler = curve.sampler();
        let gains: Vec<f64> = (0..20).map(|_| sampler.next_gain()).collect();
        assert_eq!(gains[0], 1.0);
        assert!((gains[19] - 10f64.powf(6.0 / 20.0)).abs() < 1e-12);
        // Monotone ramp between the two centers.
        for pair in gains.windows(2) {
            assert!(pair[1] >= pair[0]);
        }
        assert!(gains[9] > 1.0 && gains[9] < 10f64.powf(6.0 / 20.0));
    }

    #[test]
    fn gate_math_matches_r128() {
        // Blocks at −23 with quiet −60 blocks mixed in: the −60s fall below
        // the relative gate and must not drag the integrated value down.
        let blocks = vec![-23.0; 75].into_iter().chain(vec![-60.0; 25]);
        let (integrated, threshold) = gated_integrated(blocks);
        assert!((integrated + 23.0).abs() < 0.1, "{integrated}");
        assert!((threshold + 34.2).abs() < 1.0, "{threshold}");
    }
}
