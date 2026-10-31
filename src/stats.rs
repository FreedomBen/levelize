//! Report types, in human-readable and JSON form.

use std::fmt;
use std::path::PathBuf;

use clap::ValueEnum;
use serde_json::{Value, json};

use crate::Settings;
use crate::meter::Loudness;

/// Round for JSON output; `-inf` (silence) becomes `null`.
fn db_json(value: f64) -> Value {
    if value.is_finite() {
        json!((value * 100.0).round() / 100.0)
    } else {
        Value::Null
    }
}

/// kebab-case CLI name of a `ValueEnum` variant (`podcast`, `normal`, …).
fn value_name<T: ValueEnum>(value: T) -> String {
    value
        .to_possible_value()
        .map(|v| v.get_name().to_string())
        .unwrap_or_default()
}

/// What `--analyze` reports for one input.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzeReport {
    pub path: PathBuf,
    pub duration_secs: f64,
    pub sample_rate: u32,
    pub channels: u16,
    pub loudness: Loudness,
}

impl AnalyzeReport {
    /// The `input` object shared by both JSON report shapes.
    fn input_json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "duration_s": (self.duration_secs * 1000.0).round() / 1000.0,
            "sample_rate": self.sample_rate,
            "channels": self.channels,
            "integrated_lufs": db_json(self.loudness.integrated_lufs),
            "lra_lu": db_json(self.loudness.lra_lu),
            "true_peak_dbtp": db_json(self.loudness.true_peak_dbtp),
        })
    }

    /// One `--analyze --json` object (DESIGN.md "`--json` output").
    pub fn to_json(&self) -> Value {
        json!({
            "levelize": env!("CARGO_PKG_VERSION"),
            "input": self.input_json(),
        })
    }
}

impl fmt::Display for AnalyzeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.path.display())?;
        writeln!(f, "  duration     {}", format_duration(self.duration_secs))?;
        writeln!(
            f,
            "  format       {} Hz, {}",
            self.sample_rate,
            channel_name(self.channels)
        )?;
        writeln!(
            f,
            "  integrated   {:.1} LUFS",
            self.loudness.integrated_lufs
        )?;
        writeln!(f, "  range (LRA)  {:.1} LU", self.loudness.lra_lu)?;
        write!(f, "  true peak    {:.1} dBTP", self.loudness.true_peak_dbtp)
    }
}

/// What a leveling run reports for one input → output pair.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelReport {
    pub input: AnalyzeReport,
    pub output_name: String,
    pub output_loudness: Loudness,
    /// Median gain of the applied curve, in dB.
    pub gain_db: f64,
    /// Quietest point of the gain curve, in dB.
    pub gain_min_db: f64,
    /// Loudest point of the gain curve, in dB.
    pub gain_max_db: f64,
    /// The max-boost/max-cut caps kept the output off the target.
    pub gain_clamped: bool,
    /// No program material found; copied through with no gain.
    pub silent_input: bool,
    pub target_lufs: f64,
}

impl LevelReport {
    /// The gain bound the clamp hit, by which side of the target we ended on.
    fn cap_db(&self) -> f64 {
        if self.output_loudness.integrated_lufs < self.target_lufs {
            self.gain_max_db
        } else {
            self.gain_min_db
        }
    }

    /// Human-readable warnings; also the JSON `warnings` array.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.silent_input {
            warnings.push("no program material found; copied through with no gain".to_string());
        } else if self.gain_clamped {
            warnings.push(format!(
                "gain capped at {:+.1} dB; the {:.1} LUFS target was not reached",
                self.cap_db(),
                self.target_lufs
            ));
        }
        warnings
    }

    /// One leveling-run JSON object (DESIGN.md "`--json` output").
    pub fn to_json(&self, settings: &Settings) -> Value {
        json!({
            "levelize": env!("CARGO_PKG_VERSION"),
            "input": self.input.input_json(),
            "output": {
                "path": self.output_name,
                "integrated_lufs": db_json(self.output_loudness.integrated_lufs),
                "lra_lu": db_json(self.output_loudness.lra_lu),
                "true_peak_dbtp": db_json(self.output_loudness.true_peak_dbtp),
            },
            "gain": {
                "median_db": db_json(self.gain_db),
                "min_db": db_json(self.gain_min_db),
                "max_db": db_json(self.gain_max_db),
            },
            "settings": {
                "profile": value_name(settings.profile),
                "target_lufs": self.target_lufs,
                "ceiling_dbtp": settings.ceiling(),
                "leveling": value_name(settings.leveling),
                "format": value_name(settings.format),
            },
            "warnings": self.warnings(),
        })
    }
}

impl fmt::Display for LevelReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} → {}", self.input.path.display(), self.output_name)?;
        writeln!(
            f,
            "  integrated   {:.1} → {:.1} LUFS   (gain {:+.1} dB)",
            self.input.loudness.integrated_lufs, self.output_loudness.integrated_lufs, self.gain_db
        )?;
        if self.gain_max_db - self.gain_min_db > 0.05 {
            writeln!(
                f,
                "  gain range   {:+.1} … {:+.1} dB",
                self.gain_min_db, self.gain_max_db
            )?;
        }
        write!(
            f,
            "  true peak    {:.1} → {:.1} dBTP",
            self.input.loudness.true_peak_dbtp, self.output_loudness.true_peak_dbtp
        )?;
        for warning in self.warnings() {
            write!(f, "\n  note: {warning}")?;
        }
        Ok(())
    }
}

fn channel_name(channels: u16) -> String {
    match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{n} channels"),
    }
}

/// `h:mm:ss.s`, or `m:ss.s` under an hour.
fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.0);
    let hours = (total / 3600.0) as u64;
    let minutes = ((total % 3600.0) / 60.0) as u64;
    let secs = total % 60.0;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:04.1}")
    } else {
        format!("{minutes}:{secs:04.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_format_as_clock_time() {
        assert_eq!(format_duration(0.0), "0:00.0");
        assert_eq!(format_duration(62.5), "1:02.5");
        assert_eq!(format_duration(3723.42), "1:02:03.4");
    }

    #[test]
    fn report_renders_all_fields() {
        let report = AnalyzeReport {
            path: PathBuf::from("ep.wav"),
            duration_secs: 62.5,
            sample_rate: 48_000,
            channels: 2,
            loudness: Loudness {
                integrated_lufs: -23.04,
                lra_lu: 11.25,
                true_peak_dbtp: -3.1,
            },
        };
        let text = report.to_string();
        assert!(text.contains("ep.wav"));
        assert!(text.contains("1:02.5"));
        assert!(text.contains("48000 Hz, stereo"));
        assert!(text.contains("-23.0 LUFS"));
        assert!(text.contains("11.2 LU"));
        assert!(text.contains("-3.1 dBTP"));
    }

    #[test]
    fn level_report_shows_the_gain_range_and_cap() {
        let input = AnalyzeReport {
            path: PathBuf::from("ep.wav"),
            duration_secs: 60.0,
            sample_rate: 48_000,
            channels: 2,
            loudness: Loudness {
                integrated_lufs: -31.0,
                lra_lu: 9.0,
                true_peak_dbtp: -12.0,
            },
        };
        let report = LevelReport {
            input,
            output_name: "ep.leveled.wav".into(),
            output_loudness: Loudness {
                integrated_lufs: -17.2,
                lra_lu: 2.0,
                true_peak_dbtp: -1.8,
            },
            gain_db: 13.8,
            gain_min_db: 9.1,
            gain_max_db: 15.0,
            gain_clamped: true,
            silent_input: false,
            target_lufs: -16.0,
        };
        let text = report.to_string();
        assert!(text.contains("ep.wav → ep.leveled.wav"));
        assert!(text.contains("(gain +13.8 dB)"));
        assert!(text.contains("+9.1 … +15.0 dB"));
        assert!(text.contains("capped at +15.0 dB"));

        let flat = LevelReport {
            gain_db: 4.0,
            gain_min_db: 4.0,
            gain_max_db: 4.0,
            gain_clamped: false,
            ..report
        };
        let text = flat.to_string();
        assert!(!text.contains("gain range"));
        assert!(!text.contains("note:"));
    }

    #[test]
    fn json_reports_have_the_documented_shape() {
        let input = AnalyzeReport {
            path: PathBuf::from("ep.wav"),
            duration_secs: 62.5,
            sample_rate: 48_000,
            channels: 2,
            loudness: Loudness {
                integrated_lufs: -31.04,
                lra_lu: 9.0,
                true_peak_dbtp: -12.0,
            },
        };
        let report = LevelReport {
            input,
            output_name: "ep.leveled.wav".into(),
            output_loudness: Loudness {
                integrated_lufs: -17.2,
                lra_lu: 2.0,
                true_peak_dbtp: -1.8,
            },
            gain_db: 13.8,
            gain_min_db: 9.1,
            gain_max_db: 15.0,
            gain_clamped: true,
            silent_input: false,
            target_lufs: -16.0,
        };
        let json = report.to_json(&Settings::default());
        assert_eq!(json["levelize"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["input"]["path"], "ep.wav");
        assert_eq!(json["input"]["sample_rate"], 48_000);
        assert_eq!(json["input"]["integrated_lufs"], -31.04);
        assert_eq!(json["output"]["path"], "ep.leveled.wav");
        assert_eq!(json["gain"]["max_db"], 15.0);
        assert_eq!(json["settings"]["profile"], "podcast");
        assert_eq!(json["settings"]["leveling"], "normal");
        assert_eq!(json["settings"]["format"], "wav");
        assert_eq!(json["settings"]["target_lufs"], -16.0);
        assert!(
            json["warnings"][0].as_str().unwrap().contains("capped"),
            "{json}"
        );

        // Silence serializes as null, not as a JSON-invalid infinity.
        let silent = AnalyzeReport {
            path: PathBuf::from("quiet.wav"),
            duration_secs: 1.0,
            sample_rate: 48_000,
            channels: 1,
            loudness: Loudness {
                integrated_lufs: f64::NEG_INFINITY,
                lra_lu: 0.0,
                true_peak_dbtp: f64::NEG_INFINITY,
            },
        };
        let json = silent.to_json();
        assert!(json["input"]["integrated_lufs"].is_null());
        assert!(json["input"]["true_peak_dbtp"].is_null());
        assert!(json.get("output").is_none());
    }
}
