use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use tempfile::NamedTempFile;

use levelize::{
    AnalyzeReport, BitDepth, Format, LevelReport, Leveling, Output, Phase, Profile, Settings,
};

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
  levelize --analyze *.wav                  measure only; write nothing
  levelize --json ep.wav | jq .gain         machine-readable stats on stdout
  cat ep.wav | levelize - -o - > out.wav    filter mode: stdin to stdout"
)]
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

    /// Stats as JSON, one object per input, on stdout
    /// (on stderr when audio streams to stdout)
    #[arg(long, verbatim_doc_comment)]
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
    let format = effective_format(cli)?;
    if format == Format::Mp3 && cli.bit_depth.is_some() {
        return Err("--bit-depth does not apply to MP3 output".into());
    }
    if format == Format::Flac && cli.bit_depth == Some(BitDepth::Float32) {
        return Err("FLAC output supports --bit-depth 16 or 24".into());
    }
    if format != Format::Mp3 && cli.mp3_bitrate.is_some() {
        return Err("--mp3-bitrate requires MP3 output".into());
    }
    Ok(format)
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
    if cli.analyze {
        return analyze_all(cli);
    }
    level_all(cli, settings)
}

/// An input path the two passes can reread: `-` spills stdin to a temp file.
struct ResolvedInput {
    path: PathBuf,
    stdin: bool,
    /// Keeps the spill file alive (and deleted afterwards).
    _spill: Option<NamedTempFile>,
}

fn resolve_input(input: &Path) -> Result<ResolvedInput> {
    if input.as_os_str() != "-" {
        return Ok(ResolvedInput {
            path: input.to_owned(),
            stdin: false,
            _spill: None,
        });
    }
    let mut spill = tempfile::Builder::new()
        .prefix("levelize-stdin-")
        .tempfile()
        .context("creating a temp file for stdin")?;
    std::io::copy(&mut std::io::stdin().lock(), spill.as_file_mut())
        .context("spilling stdin to a temp file")?;
    Ok(ResolvedInput {
        path: spill.path().to_owned(),
        stdin: true,
        _spill: Some(spill),
    })
}

/// Level each input independently; report per-file errors and keep going.
fn level_all(cli: &Cli, settings: &Settings) -> Result<()> {
    let to_stdout = cli.output.as_deref().is_some_and(|p| p.as_os_str() == "-");
    let mut failures = 0usize;
    for input in &cli.inputs {
        match level_one(cli, settings, input) {
            Ok(report) => {
                if cli.json {
                    let line = report.to_json(settings);
                    if to_stdout {
                        eprintln!("{line}");
                    } else {
                        println!("{line}");
                    }
                } else if !cli.quiet {
                    eprintln!("{report}");
                }
                if cli.json || cli.quiet {
                    for warning in report.warnings() {
                        eprintln!("levelize: {}: warning: {warning}", input.display());
                    }
                }
            }
            Err(error) => {
                failures += 1;
                eprintln!("levelize: {}", prefixed_error(input, &error));
            }
        }
    }
    if failures > 0 {
        bail!("{failures} of {} input(s) failed", cli.inputs.len());
    }
    Ok(())
}

/// `{input}: {error}` — unless the error already leads with the path, as
/// decode errors do; `a.wav: a.wav: …` helps nobody.
fn prefixed_error(input: &Path, error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    let path = input.display().to_string();
    if text.starts_with(&path) {
        text
    } else {
        format!("{path}: {text}")
    }
}

fn level_one(cli: &Cli, settings: &Settings, input: &Path) -> Result<LevelReport> {
    let resolved = resolve_input(input)?;
    let bar = progress_bar(cli);
    let progress = |phase: Phase, done: f64, total: Option<f64>| {
        if let Some(bar) = &bar {
            drive_bar(bar, phase, done, total);
        }
    };

    let to_stdout = cli.output.as_deref().is_some_and(|p| p.as_os_str() == "-");
    let mut result = if to_stdout {
        levelize::level_with_progress(&resolved.path, Output::Stdout, settings, progress)
    } else {
        let output = match cli.output.as_deref() {
            Some(path) => path.to_owned(),
            None => default_output_path(input, settings.format),
        };
        check_output_path(&resolved.path, &output, cli.force)?;
        let result = levelize::level_with_progress(
            &resolved.path,
            Output::File(&output),
            settings,
            progress,
        );
        if result.is_err() {
            // Best effort: don't leave a partial file behind.
            let _ = std::fs::remove_file(&output);
        }
        result
    };
    if let Some(bar) = &bar {
        bar.finish_and_clear();
    }
    if resolved.stdin
        && let Ok(report) = &mut result
    {
        report.input.path = PathBuf::from("-");
    }
    result
}

/// A stderr progress bar, unless quiet or stderr isn't a terminal.
fn progress_bar(cli: &Cli) -> Option<ProgressBar> {
    if cli.quiet || !std::io::stderr().is_terminal() {
        return None;
    }
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    bar.set_style(
        ProgressStyle::with_template("{spinner} {msg:>9} {elapsed}").expect("static template"),
    );
    Some(bar)
}

/// Feed one `level_with_progress` callback into the bar. Positions are in
/// tenths of a second; the two passes split the bar half and half.
fn drive_bar(bar: &ProgressBar, phase: Phase, done_secs: f64, total_secs: Option<f64>) {
    const SCALE: f64 = 10.0;
    bar.set_message(match phase {
        Phase::Analyze => "analyzing",
        Phase::Render => "rendering",
    });
    match total_secs {
        Some(total) => {
            let length = ((2.0 * total * SCALE) as u64).max(1);
            if bar.length() != Some(length) {
                bar.set_style(
                    ProgressStyle::with_template("{msg:>9} [{wide_bar}] {percent:>3}%  {eta}")
                        .expect("static template")
                        .progress_chars("=> "),
                );
                bar.set_length(length);
            }
            let offset = match phase {
                Phase::Analyze => 0.0,
                Phase::Render => total,
            };
            bar.set_position(((offset + done_secs) * SCALE) as u64);
        }
        None => bar.set_position((done_secs * SCALE) as u64),
    }
}

/// `dir/episode.wav` → `dir/episode.leveled.<ext>`.
fn default_output_path(input: &Path, format: Format) -> PathBuf {
    let ext = match format {
        Format::Wav => "wav",
        Format::Flac => "flac",
        Format::Mp3 => "mp3",
    };
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    input.with_file_name(format!("{stem}.leveled.{ext}"))
}

fn check_output_path(input: &Path, output: &Path, force: bool) -> Result<()> {
    let absolute_input = std::path::absolute(input).unwrap_or_else(|_| input.to_owned());
    let absolute_output = std::path::absolute(output).unwrap_or_else(|_| output.to_owned());
    if absolute_input == absolute_output {
        // Pass 2 re-reads the input while writing, so this can never be allowed.
        bail!("output would overwrite the input; choose a different path");
    }
    if output.exists() && !force {
        bail!("{} exists; pass --force to overwrite", output.display());
    }
    Ok(())
}

/// Analyze each input independently; report per-file errors and keep going.
fn analyze_all(cli: &Cli) -> Result<()> {
    let mut failures = 0usize;
    for (index, input) in cli.inputs.iter().enumerate() {
        if index > 0 && !cli.json {
            println!();
        }
        match analyze_one(input) {
            Ok(report) => {
                if cli.json {
                    println!("{}", report.to_json());
                } else {
                    println!("{report}");
                }
            }
            Err(error) => {
                failures += 1;
                eprintln!("levelize: {}", prefixed_error(input, &error));
            }
        }
    }
    if failures > 0 {
        bail!(
            "{failures} of {} input(s) failed to analyze",
            cli.inputs.len()
        );
    }
    Ok(())
}

fn analyze_one(input: &Path) -> Result<AnalyzeReport> {
    let resolved = resolve_input(input)?;
    let mut report = levelize::analyze(&resolved.path)?;
    if resolved.stdin {
        report.path = PathBuf::from("-");
    }
    Ok(report)
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
    fn error_prefix_is_not_doubled() {
        let input = Path::new("ep.wav");
        let named = anyhow::anyhow!("ep.wav: 4 channels; levelize handles mono and stereo only");
        assert_eq!(
            prefixed_error(input, &named),
            "ep.wav: 4 channels; levelize handles mono and stereo only"
        );
        let anonymous = anyhow::anyhow!("writing WAV data");
        assert_eq!(
            prefixed_error(input, &anonymous),
            "ep.wav: writing WAV data"
        );
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
    fn bit_depth_is_rejected_for_mp3_output() {
        let cli = parse(&["levelize", "--bit-depth", "24", "-o", "out.mp3", "in.wav"]);
        let error = validate_args(&cli).unwrap_err();
        assert!(error.contains("MP3"), "{error}");
    }

    #[test]
    fn float_bit_depth_is_rejected_for_flac_output() {
        let cli = parse(&["levelize", "--bit-depth", "32f", "-o", "out.flac", "in.wav"]);
        let error = validate_args(&cli).unwrap_err();
        assert!(error.contains("16 or 24"), "{error}");

        let cli = parse(&["levelize", "--bit-depth", "24", "-o", "out.flac", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::Flac));
    }

    #[test]
    fn mp3_bitrate_needs_mp3_output() {
        let cli = parse(&["levelize", "--mp3-bitrate", "96", "in.wav"]);
        let error = validate_args(&cli).unwrap_err();
        assert!(error.contains("--mp3-bitrate"), "{error}");

        let cli = parse(&["levelize", "--mp3-bitrate", "96", "-o", "out.mp3", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::Mp3));
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

    #[test]
    fn default_output_names_follow_the_input() {
        assert_eq!(
            default_output_path(Path::new("ep.wav"), Format::Wav),
            PathBuf::from("ep.leveled.wav")
        );
        assert_eq!(
            default_output_path(Path::new("dir/ep.flac"), Format::Mp3),
            PathBuf::from("dir/ep.leveled.mp3")
        );
        assert_eq!(
            default_output_path(Path::new("noext"), Format::Flac),
            PathBuf::from("noext.leveled.flac")
        );
    }

    #[test]
    fn output_path_checks_guard_the_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.wav");
        let output = dir.path().join("out.wav");
        std::fs::write(&input, b"x").unwrap();

        // Same path is refused even with --force.
        assert!(check_output_path(&input, &input, true).is_err());
        // A fresh path is fine.
        assert!(check_output_path(&input, &output, false).is_ok());
        // An existing path needs --force.
        std::fs::write(&output, b"x").unwrap();
        assert!(check_output_path(&input, &output, false).is_err());
        assert!(check_output_path(&input, &output, true).is_ok());
    }
}
