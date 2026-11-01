//! The clap command-line definition, in the library so the `levelize-man`
//! binary (feature `mangen`) can generate the man page from it. The CLI
//! behavior itself lives in `src/main.rs`.

use std::path::PathBuf;

use clap::Parser;

use crate::{BitDepth, Format, Leveling, Profile};

/// Adaptive loudness leveling for spoken-word audio.
///
/// Levels speech between and within speakers — quiet voices up, loud voices
/// down, pauses left alone — then lands the result on a standard loudness
/// target under a true-peak ceiling. `levelize episode.wav` writes
/// `episode.leveled.wav` next to the input.
#[derive(Parser, Debug)]
#[command(
    version,
    about,
    after_help = "\
Examples:
  levelize episode.wav                      write episode.leveled.wav at the podcast target
  levelize --profile audiobook book.flac    ACX-friendly: -20 LUFS under a -3 dBTP ceiling
  levelize ep.mp3 -o ep.leveled.mp3         MP3 in and out (MP3 output runs `lame`)
  levelize book.flac -o book.m4b \\
           --chapters toc.txt               M4B audiobook with chapter marks (runs `ffmpeg`)
  levelize --analyze *.wav                  measure only; write nothing
  levelize --json ep.wav | jq .gain         machine-readable stats on stdout
  cat ep.wav | levelize - -o - > out.wav    filter mode: stdin to stdout"
)]
pub struct Cli {
    /// Input files; `-` reads stdin
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<PathBuf>,

    /// Output file, or `-` for stdout (single input only)
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Output format [default: from the -o extension, else wav]
    #[arg(long, value_enum, value_name = "FMT")]
    pub format: Option<Format>,

    /// Loudness preset for target and ceiling
    #[arg(long, value_enum, default_value = "podcast", value_name = "NAME")]
    pub profile: Profile,

    /// Integrated loudness target in LUFS; overrides the profile
    #[arg(long, value_name = "LUFS", allow_negative_numbers = true)]
    pub target: Option<f64>,

    /// True-peak ceiling in dBTP; overrides the profile
    #[arg(long, value_name = "DBTP", allow_negative_numbers = true)]
    pub ceiling: Option<f64>,

    /// Cap on upward gain in dB (protects the noise floor)
    #[arg(long, value_name = "DB", default_value_t = 15.0)]
    pub max_boost: f64,

    /// Cap on downward gain in dB
    #[arg(long, value_name = "DB", default_value_t = 15.0)]
    pub max_cut: f64,

    /// How hard levels converge on the target
    #[arg(long, value_enum, default_value = "normal", value_name = "MODE")]
    pub leveling: Leveling,

    /// Output sample format for WAV/FLAC [default: match the input]
    #[arg(long, value_enum, value_name = "DEPTH")]
    pub bit_depth: Option<BitDepth>,

    /// CBR bitrate for MP3 output [default: a lossy input's own bitrate, else 128 stereo / 96 mono]
    #[arg(long, value_name = "KBPS")]
    pub mp3_bitrate: Option<u32>,

    /// Bitrate for M4B/M4A output [default: a lossy input's own bitrate, else 96 stereo / 64 mono]
    #[arg(long, value_name = "KBPS")]
    pub aac_bitrate: Option<u32>,

    /// Chapter marks for M4B/M4A output: a text file with one chapter per
    /// line as `TIME Title`, where TIME is [H:]MM:SS[.sss] or plain seconds
    /// (`#` comments and blank lines are skipped); replaces any chapters
    /// found in the input
    #[arg(long, value_name = "FILE", verbatim_doc_comment)]
    pub chapters: Option<PathBuf>,

    /// Don't carry chapter marks from the input into M4B/M4A output
    #[arg(long, conflicts_with = "chapters")]
    pub no_chapters: bool,

    /// Analysis pass only: print loudness stats, write nothing
    #[arg(long, conflicts_with = "output")]
    pub analyze: bool,

    /// Stats as JSON, one object per input, on stdout
    /// (on stderr when audio streams to stdout)
    #[arg(long, verbatim_doc_comment)]
    pub json: bool,

    /// Allow overwriting an existing output file
    #[arg(long)]
    pub force: bool,

    /// Errors only on stderr
    #[arg(short, long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Detailed progress on stderr
    #[arg(short, long)]
    pub verbose: bool,
}

#[cfg(all(test, feature = "mangen"))]
mod man_tests {
    use clap::CommandFactory;

    #[test]
    fn man_page_renders_from_the_cli_definition() {
        let mut roff = Vec::new();
        clap_mangen::Man::new(super::Cli::command())
            .render(&mut roff)
            .unwrap();
        let text = String::from_utf8(roff).unwrap();
        let th = text
            .lines()
            .find(|line| line.starts_with(".TH"))
            .expect("a .TH title line");
        assert!(th.starts_with(".TH levelize 1"), "{th}");
        assert!(th.contains(env!("CARGO_PKG_VERSION")), "{th}");
        // Roff escapes hyphens, so probe with hyphen-free phrases.
        assert!(text.contains("analyze"));
        assert!(text.contains("between and within speakers"));
    }
}
