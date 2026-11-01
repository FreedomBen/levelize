use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use tempfile::NamedTempFile;

use levelize::cli::Cli;
use levelize::encode::{Chapter, load_chapters};
use levelize::{AnalyzeReport, BitDepth, Format, LevelReport, Output, Phase, Settings};

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
    if (format == Format::Mp3 || format.is_aac()) && cli.bit_depth.is_some() {
        return Err(format!(
            "--bit-depth does not apply to {} output",
            format.label()
        ));
    }
    if format == Format::Flac && cli.bit_depth == Some(BitDepth::Float32) {
        return Err("FLAC output supports --bit-depth 16 or 24".into());
    }
    if format != Format::Mp3 && cli.mp3_bitrate.is_some() {
        return Err("--mp3-bitrate requires MP3 output".into());
    }
    if !format.is_aac() && cli.aac_bitrate.is_some() {
        return Err("--aac-bitrate requires M4B or M4A output".into());
    }
    if cli.chapters.is_some() {
        if cli.analyze || !format.is_aac() {
            return Err("--chapters requires M4B or M4A output".into());
        }
        if cli.inputs.len() > 1 {
            return Err("--chapters takes a single input; the marks name one recording".into());
        }
    }
    if cli.no_chapters && (cli.analyze || !format.is_aac()) {
        return Err("--no-chapters requires M4B or M4A output".into());
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
        Some("m4b") => Ok(Format::M4b),
        Some("m4a") => Ok(Format::M4a),
        _ => Err(format!(
            "cannot infer an output format from `{}`; pass --format wav|flac|mp3|m4b|m4a",
            path.display()
        )),
    }
}

fn build_settings(cli: &Cli, format: Format, chapters: Vec<Chapter>) -> Settings {
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
        aac_bitrate_kbps: cli.aac_bitrate,
        chapters,
        keep_chapters: !cli.no_chapters,
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
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    input.with_file_name(format!("{stem}.leveled.{}", format.extension()))
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
    // A bad chapters file blocks the whole run, so it is a usage-class error.
    let chapters = match cli.chapters.as_deref().map(load_chapters).transpose() {
        Ok(chapters) => chapters.unwrap_or_default(),
        Err(error) => Cli::command()
            .error(ErrorKind::ValueValidation, format!("{error:#}"))
            .exit(),
    };
    let settings = build_settings(&cli, format, chapters);
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
    use levelize::{Leveling, Profile};

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
        let settings = build_settings(&cli, Format::Wav, Vec::new());
        assert_eq!(settings.target_lufs, Some(-20.0));
        assert_eq!(settings.max_boost_db, 9.0);
        assert_eq!(settings.target_for(2), -20.0);
    }

    #[test]
    fn m4b_and_m4a_follow_the_output_extension() {
        let cli = parse(&["levelize", "-o", "book.m4b", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::M4b));

        let cli = parse(&["levelize", "-o", "EP.M4A", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::M4a));
    }

    #[test]
    fn bit_depth_is_rejected_for_m4b_output() {
        let cli = parse(&["levelize", "--bit-depth", "24", "-o", "out.m4b", "in.wav"]);
        let error = validate_args(&cli).unwrap_err();
        assert!(error.contains("M4B"), "{error}");
    }

    #[test]
    fn aac_bitrate_needs_m4b_or_m4a_output() {
        let cli = parse(&["levelize", "--aac-bitrate", "64", "in.wav"]);
        let error = validate_args(&cli).unwrap_err();
        assert!(error.contains("--aac-bitrate"), "{error}");

        for output in ["out.m4b", "out.m4a"] {
            let cli = parse(&["levelize", "--aac-bitrate", "64", "-o", output, "in.wav"]);
            assert!(validate_args(&cli).is_ok(), "{output}");
        }
    }

    #[test]
    fn chapters_need_one_input_and_aac_output() {
        let cli = parse(&[
            "levelize",
            "--chapters",
            "toc.txt",
            "-o",
            "out.wav",
            "in.wav",
        ]);
        let error = validate_args(&cli).unwrap_err();
        assert!(error.contains("M4B or M4A"), "{error}");

        let cli = parse(&[
            "levelize",
            "--chapters",
            "toc.txt",
            "--format",
            "m4b",
            "a.wav",
            "b.wav",
        ]);
        let error = validate_args(&cli).unwrap_err();
        assert!(error.contains("single input"), "{error}");

        let cli = parse(&["levelize", "--chapters", "toc.txt", "--analyze", "in.wav"]);
        assert!(validate_args(&cli).is_err());

        let cli = parse(&[
            "levelize",
            "--chapters",
            "toc.txt",
            "-o",
            "out.m4b",
            "in.wav",
        ]);
        assert_eq!(validate_args(&cli), Ok(Format::M4b));
    }

    #[test]
    fn no_chapters_conflicts_with_chapters_and_needs_aac_output() {
        assert!(
            Cli::try_parse_from([
                "levelize",
                "--chapters",
                "toc.txt",
                "--no-chapters",
                "-o",
                "out.m4b",
                "in.wav",
            ])
            .is_err()
        );

        let cli = parse(&["levelize", "--no-chapters", "in.wav"]);
        let error = validate_args(&cli).unwrap_err();
        assert!(error.contains("M4B or M4A"), "{error}");

        let cli = parse(&["levelize", "--no-chapters", "-o", "out.m4a", "in.wav"]);
        assert_eq!(validate_args(&cli), Ok(Format::M4a));
        assert!(!build_settings(&cli, Format::M4a, Vec::new()).keep_chapters);
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
        assert_eq!(
            default_output_path(Path::new("book.wav"), Format::M4b),
            PathBuf::from("book.leveled.m4b")
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
