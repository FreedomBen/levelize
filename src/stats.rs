//! Report types: human-readable now, JSON in M5.

use std::fmt;
use std::path::PathBuf;

use crate::meter::Loudness;

/// What `--analyze` reports for one input.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzeReport {
    pub path: PathBuf,
    pub duration_secs: f64,
    pub sample_rate: u32,
    pub channels: u16,
    pub loudness: Loudness,
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
        if self.silent_input {
            write!(f, "\n  note: no program material found; no gain applied")?;
        } else if self.gain_clamped {
            let cap = if self.output_loudness.integrated_lufs < self.target_lufs {
                self.gain_max_db
            } else {
                self.gain_min_db
            };
            write!(
                f,
                "\n  note: gain capped at {:+.1} dB; the {:.1} LUFS target was not reached",
                cap, self.target_lufs
            )?;
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
}
