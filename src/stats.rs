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
}
