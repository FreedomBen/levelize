use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

use levelize::{BitDepth, Format, Leveling, Profile, Settings};

/// Adaptive loudness leveling for spoken-word audio.
///
/// Levels speech between and within speakers — quiet voices up, loud voices
/// down, pauses left alone — then lands the result on a standard loudness
/// target under a true-peak ceiling. `levelize episode.wav` writes
/// `episode.leveled.wav` next to the input.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Input files; `-` reads stdin
    #[arg(required = true, value_name = "INPUT")]
    inputs: Vec<PathBuf>,

    /// Output file, or `-` for stdout (single input only)
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Output format [default: from the -o extension, else wav]
    #[arg(long, value_enum, value_name = "FMT")]
    format: Option<Format>,

    /// Loudness preset for target and ceiling
    #[arg(long, value_enum, default_value = "podcast", value_name = "NAME")]
    profile: Profile,

    /// Integrated loudness target in LUFS; overrides the profile
    #[arg(long, value_name = "LUFS", allow_negative_numbers = true)]
    target: Option<f64>,

    /// True-peak ceiling in dBTP; overrides the profile
    #[arg(long, value_name = "DBTP", allow_negative_numbers = true)]
    ceiling: Option<f64>,

    /// Cap on upward gain in dB (protects the noise floor)
    #[arg(long, value_name = "DB", default_value_t = 15.0)]
    max_boost: f64,

    /// Cap on downward gain in dB
    #[arg(long, value_name = "DB", default_value_t = 15.0)]
    max_cut: f64,

    /// How hard levels converge on the target
    #[arg(long, value_enum, default_value = "normal", value_name = "MODE")]
    leveling: Leveling,

    /// Output sample format for WAV/FLAC [default: match the input]
    #[arg(long, value_enum, value_name = "DEPTH")]
    bit_depth: Option<BitDepth>,

    /// CBR bitrate for MP3 output [default: 128 stereo / 96 mono]
    #[arg(long, value_name = "KBPS")]
    mp3_bitrate: Option<u32>,

    /// Analysis pass only: print loudness stats, write nothing
    #[arg(long, conflicts_with = "output")]
    analyze: bool,

    /// Emit stats as JSON on stdout
    #[arg(long)]
    json: bool,

    /// Allow overwriting an existing output file
    #[arg(long)]
    force: bool,

    /// Errors only on stderr
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,

    /// Detailed progress on stderr
    #[arg(short, long)]
    verbose: bool,
}

/// Cross-argument rules clap can't express; `Err` values are usage errors.
fn validate_args(cli: &Cli) -> Result<Format, String> {
    if cli.output.is_some() && cli.inputs.len() > 1 {
        return Err("--output takes a single input; drop -o to derive output names".into());
    }
    let has_stdin = cli.inputs.iter().any(|path| path.as_os_str() == "-");
    if has_stdin && cli.inputs.len() > 1 {
        return Err("stdin (`-`) cannot be combined with other inputs".into());
    }
    if has_stdin && cli.output.is_none() && !cli.analyze {
        return Err("stdin input needs --output (or --analyze): there is no name to derive".into());
    }
    effective_format(cli)
}

/// Output format: the explicit flag, else the `-o` extension, else WAV.
fn effective_format(cli: &Cli) -> Result<Format, String> {
    if let Some(format) = cli.format {
        return Ok(format);
    }
    let Some(path) = cli.output.as_deref() else {
        return Ok(Format::Wav);
    };
    if path.as_os_str() == "-" {
        return Ok(Format::Wav);
    }
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("wav") => Ok(Format::Wav),
        Some("flac") => Ok(Format::Flac),
        Some("mp3") => Ok(Format::Mp3),
        _ => Err(format!(
            "cannot infer an output format from `{}`; pass --format wav|flac|mp3",
            path.display()
        )),
    }
}

fn build_settings(cli: &Cli, format: Format) -> Settings {
    Settings {
        profile: cli.profile,
        target_lufs: cli.target,
        ceiling_dbtp: cli.ceiling,
        max_boost_db: cli.max_boost,
        max_cut_db: cli.max_cut,
        leveling: cli.leveling,
        format,
        bit_depth: cli.bit_depth,
        mp3_bitrate_kbps: cli.mp3_bitrate,
    }
}

fn run(cli: &Cli, settings: &Settings) -> Result<()> {
    if cli.verbose {
        eprintln!("levelize: settings: {settings:?}");
        eprintln!(
            "levelize: inputs: {:?}, output: {:?}, analyze={}, json={}, force={}, quiet={}",
            cli.inputs, cli.output, cli.analyze, cli.json, cli.force, cli.quiet
        );
    }
    bail!(
        "nothing past argument parsing is implemented yet — the meter lands with M1 (see DESIGN.md milestones)"
    )
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let format = match validate_args(&cli) {
        Ok(format) => format,
        Err(message) => Cli::command()
            .error(ErrorKind::ValueValidation, message)
            .exit(),
    };
    let settings = build_settings(&cli, format);
    match run(&cli, &settings) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("levelize: error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("args should parse")
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn defaults_match_the_design_doc() {
        let cli = parse(&["levelize", "in.wav"]);
        assert_eq!(cli.profile, Profile::Podcast);
        assert_eq!(cli.leveling, Leveling::Normal);
        assert_eq!(cli.max_boost, 15.0);
        assert_eq!(cli.max_cut, 15.0);
        assert_eq!(validate_args(&cli), Ok(Format::Wav));
    }

    #[test]
    fn negative_loudness_values_parse() {
        let cli = parse(&["levelize", "--target", "-19", "--ceiling", "-1", "in.wav"]);
        assert_eq!(cli.target, Some(-19.0));
        assert_eq!(cli.ceiling, Some(-1.0));
    }

    #[test]
    fn format_follows_output_extension() {
        let cli = parse(&["levelize", "-o", "out.flac", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::Flac));

        let cli = parse(&["levelize", "-o", "OUT.MP3", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::Mp3));
    }

    #[test]
    fn explicit_format_beats_extension() {
        let cli = parse(&["levelize", "--format", "flac", "-o", "out.wav", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::Flac));
    }

    #[test]
    fn unknown_extension_needs_explicit_format() {
        let cli = parse(&["levelize", "-o", "out.aiff", "in.wav"]);
        assert!(validate_args(&cli).is_err());
    }

    #[test]
    fn stdout_output_defaults_to_wav() {
        let cli = parse(&["levelize", "-o", "-", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::Wav));
    }

    #[test]
    fn output_requires_a_single_input() {
        let cli = parse(&["levelize", "-o", "out.wav", "a.wav", "b.wav"]);
        assert!(validate_args(&cli).is_err());
    }

    #[test]
    fn stdin_needs_output_or_analyze() {
        let cli = parse(&["levelize", "-"]);
        assert!(validate_args(&cli).is_err());

        let cli = parse(&["levelize", "--analyze", "-"]);
        assert!(validate_args(&cli).is_ok());

        let cli = parse(&["levelize", "-o", "out.wav", "-"]);
        assert!(validate_args(&cli).is_ok());
    }

    #[test]
    fn analyze_conflicts_with_output() {
        assert!(Cli::try_parse_from(["levelize", "--analyze", "-o", "x.wav", "in.wav"]).is_err());
    }

    #[test]
    fn settings_carry_cli_values() {
        let cli = parse(&["levelize", "--target", "-20", "--max-boost", "9", "in.wav"]);
        let settings = build_settings(&cli, Format::Wav);
        assert_eq!(settings.target_lufs, Some(-20.0));
        assert_eq!(settings.max_boost_db, 9.0);
        assert_eq!(settings.target_for(2), -20.0);
    }
}
