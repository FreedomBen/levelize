//! Core engine for levelize — adaptive loudness leveling for spoken-word audio.
//!
//! See DESIGN.md: two streaming passes over the input. Pass 1 measures
//! BS.1770 loudness and plans a gain curve; pass 2 applies it, limits true
//! peaks, and encodes. The engine modules (decode, meter, plan, render,
//! encode) land with milestones M1–M4; this crate root holds the shared
//! configuration surface.

use clap::ValueEnum;

/// Loudness preset: supplies the default target and ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Profile {
    /// −16 LUFS stereo / −19 mono, −1.5 dBTP (Apple Podcasts)
    Podcast,
    /// −23 LUFS, −1 dBTP (EBU R128)
    Broadcast,
    /// −20 LUFS, −3 dBTP (lands inside ACX's RMS/peak bounds)
    Audiobook,
}

impl Profile {
    /// Default integrated-loudness target in LUFS for a channel count.
    pub fn target_lufs(self, channels: u16) -> f64 {
        match self {
            Profile::Podcast => {
                if channels == 1 {
                    -19.0
                } else {
                    -16.0
                }
            }
            Profile::Broadcast => -23.0,
            Profile::Audiobook => -20.0,
        }
    }

    /// Default true-peak ceiling in dBTP.
    pub fn ceiling_dbtp(self) -> f64 {
        match self {
            Profile::Podcast => -1.5,
            Profile::Broadcast => -1.0,
            Profile::Audiobook => -3.0,
        }
    }
}

/// How hard the gain curve converges on the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Leveling {
    Gentle,
    Normal,
    Aggressive,
}

/// Output container/codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Wav,
    Flac,
    /// Requires `lame` in PATH at run time.
    Mp3,
}

/// Output sample format for WAV/FLAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BitDepth {
    #[value(name = "16")]
    Int16,
    #[value(name = "24")]
    Int24,
    /// 32-bit float (WAV only)
    #[value(name = "32f")]
    Float32,
}

/// Full configuration for one leveling run.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub profile: Profile,
    /// Explicit target in LUFS; overrides the profile when set.
    pub target_lufs: Option<f64>,
    /// Explicit ceiling in dBTP; overrides the profile when set.
    pub ceiling_dbtp: Option<f64>,
    /// Cap on upward gain in dB.
    pub max_boost_db: f64,
    /// Cap on downward gain in dB.
    pub max_cut_db: f64,
    pub leveling: Leveling,
    pub format: Format,
    /// `None` matches the input's bit depth.
    pub bit_depth: Option<BitDepth>,
    /// CBR bitrate for MP3 output; `None` picks 128 kbps stereo / 96 mono.
    pub mp3_bitrate_kbps: Option<u32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            profile: Profile::Podcast,
            target_lufs: None,
            ceiling_dbtp: None,
            max_boost_db: 15.0,
            max_cut_db: 15.0,
            leveling: Leveling::Normal,
            format: Format::Wav,
            bit_depth: None,
            mp3_bitrate_kbps: None,
        }
    }
}

impl Settings {
    /// Effective loudness target for an input with `channels` channels.
    pub fn target_for(&self, channels: u16) -> f64 {
        self.target_lufs
            .unwrap_or_else(|| self.profile.target_lufs(channels))
    }

    /// Effective true-peak ceiling.
    pub fn ceiling(&self) -> f64 {
        self.ceiling_dbtp
            .unwrap_or_else(|| self.profile.ceiling_dbtp())
    }

    /// Effective MP3 bitrate in kbps for an input with `channels` channels.
    pub fn mp3_bitrate_for(&self, channels: u16) -> u32 {
        self.mp3_bitrate_kbps
            .unwrap_or(if channels == 1 { 96 } else { 128 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn podcast_target_depends_on_channel_count() {
        assert_eq!(Profile::Podcast.target_lufs(2), -16.0);
        assert_eq!(Profile::Podcast.target_lufs(1), -19.0);
    }

    #[test]
    fn broadcast_and_audiobook_targets_ignore_channel_count() {
        assert_eq!(Profile::Broadcast.target_lufs(1), -23.0);
        assert_eq!(Profile::Broadcast.target_lufs(2), -23.0);
        assert_eq!(Profile::Audiobook.target_lufs(1), -20.0);
        assert_eq!(Profile::Audiobook.target_lufs(2), -20.0);
    }

    #[test]
    fn profile_ceilings_match_the_design_doc() {
        assert_eq!(Profile::Podcast.ceiling_dbtp(), -1.5);
        assert_eq!(Profile::Broadcast.ceiling_dbtp(), -1.0);
        assert_eq!(Profile::Audiobook.ceiling_dbtp(), -3.0);
    }

    #[test]
    fn explicit_values_override_the_profile() {
        let settings = Settings {
            target_lufs: Some(-14.0),
            ceiling_dbtp: Some(-2.0),
            ..Settings::default()
        };
        assert_eq!(settings.target_for(1), -14.0);
        assert_eq!(settings.ceiling(), -2.0);
    }

    #[test]
    fn profile_supplies_defaults_when_not_overridden() {
        let settings = Settings {
            profile: Profile::Broadcast,
            ..Settings::default()
        };
        assert_eq!(settings.target_for(2), -23.0);
        assert_eq!(settings.ceiling(), -1.0);
    }

    #[test]
    fn mp3_bitrate_defaults_follow_channel_count() {
        let settings = Settings::default();
        assert_eq!(settings.mp3_bitrate_for(1), 96);
        assert_eq!(settings.mp3_bitrate_for(2), 128);

        let settings = Settings {
            mp3_bitrate_kbps: Some(64),
            ..Settings::default()
        };
        assert_eq!(settings.mp3_bitrate_for(2), 64);
    }
}
