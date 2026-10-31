//! End-to-end tests against the built binary: pipes, JSON, exit codes.

mod common;

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::{sine, write_wav_pcm16};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_levelize"))
}

fn run_with_stdin(mut command: Command, stdin: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn quiet_wav(path: &Path) {
    write_wav_pcm16(path, 48_000, 2, &sine(48_000, 2, 997.0, -30.0, 6.0));
}

#[test]
fn filter_mode_streams_wav_and_puts_json_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    quiet_wav(&input);
    let bytes = std::fs::read(&input).unwrap();

    let mut command = bin();
    command.args(["-", "-o", "-", "--json"]);
    let output = run_with_stdin(command, &bytes);
    assert!(output.status.success(), "{output:?}");

    // stdout carries the leveled WAV.
    let leveled = dir.path().join("out.wav");
    std::fs::write(&leveled, &output.stdout).unwrap();
    let report = levelize::analyze(&leveled).unwrap();
    assert!(
        (report.loudness.integrated_lufs + 16.0).abs() <= 0.5,
        "{}",
        report.loudness.integrated_lufs
    );

    // stderr carries one JSON object for the run.
    let stderr = String::from_utf8(output.stderr).unwrap();
    let line = stderr
        .lines()
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON on stderr: {stderr}"));
    let json: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(json["input"]["path"], "-");
    assert_eq!(json["output"]["path"], "-");
    assert_eq!(json["settings"]["profile"], "podcast");
    assert!(json["gain"]["median_db"].as_f64().unwrap() > 13.0);
    assert_eq!(json["warnings"].as_array().unwrap().len(), 0);
}

#[test]
fn analyze_json_prints_one_object_per_input() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.wav");
    let b = dir.path().join("b.wav");
    quiet_wav(&a);
    quiet_wav(&b);

    let output = bin()
        .arg("--analyze")
        .arg("--json")
        .arg(&a)
        .arg(&b)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    for line in lines {
        let json: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(json["input"]["integrated_lufs"].as_f64().is_some());
        assert!(json.get("output").is_none());
    }
}

#[test]
fn capped_gain_warns_in_json_and_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output_path = dir.path().join("out.wav");
    write_wav_pcm16(&input, 48_000, 2, &sine(48_000, 2, 997.0, -45.0, 6.0));

    let output = bin()
        .arg("--json")
        .arg("-o")
        .arg(&output_path)
        .arg(&input)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    let warning = json["warnings"][0].as_str().unwrap();
    assert!(warning.contains("capped"), "{warning}");
    assert_eq!(json["gain"]["median_db"], 15.0);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("warning"), "{stderr}");
}

#[test]
fn refusing_to_overwrite_exits_with_code_one() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output_path = dir.path().join("out.wav");
    quiet_wav(&input);

    let first = bin()
        .arg("-o")
        .arg(&output_path)
        .arg(&input)
        .output()
        .unwrap();
    assert!(first.status.success(), "{first:?}");

    let second = bin()
        .arg("-o")
        .arg(&output_path)
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1), "{second:?}");
    let stderr = String::from_utf8(second.stderr).unwrap();
    assert!(stderr.contains("--force"), "{stderr}");

    let forced = bin()
        .arg("--force")
        .arg("-o")
        .arg(&output_path)
        .arg(&input)
        .output()
        .unwrap();
    assert!(forced.status.success(), "{forced:?}");
}

#[test]
fn usage_errors_exit_with_code_two() {
    let output = bin()
        .arg("in.wav")
        .arg("--mp3-bitrate")
        .arg("96")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn a_failing_input_does_not_stop_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    let garbage = dir.path().join("garbage.wav");
    std::fs::write(&garbage, b"not audio").unwrap();
    let good = dir.path().join("good.wav");
    quiet_wav(&good);

    let output = bin().arg(&garbage).arg(&good).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(dir.path().join("good.leveled.wav").exists());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("1 of 2 input(s) failed"), "{stderr}");
    // The input path leads the error line exactly once, not doubled.
    let line = stderr.lines().find(|l| l.contains("garbage.wav")).unwrap();
    let path = garbage.display().to_string();
    assert_eq!(line.matches(&path).count(), 1, "{line}");
}

#[test]
fn analyze_reads_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    quiet_wav(&input);
    let bytes = std::fs::read(&input).unwrap();

    let mut command = bin();
    command.args(["--analyze", "-"]);
    let output = run_with_stdin(command, &bytes);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with('-'), "{stdout}");
    assert!(stdout.contains("-30.0 LUFS"), "{stdout}");
}
