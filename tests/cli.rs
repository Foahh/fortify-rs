use std::{
    fs,
    process::{Command, Output},
};
use tempfile::TempDir;

fn run(temp: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fortify"))
        .current_dir(temp.path())
        .args(["-c", "config.toml"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn configuration_init_validate_reset_and_backup() {
    let temp = TempDir::new().unwrap();

    assert!(
        run(&temp, &["config", "init", "-d", "Downloads"])
            .status
            .success()
    );
    let content = fs::read_to_string(temp.path().join("config.toml")).unwrap();

    assert!(content.contains("directory"));
    assert!(!run(&temp, &["config", "init"]).status.success());
    assert!(run(&temp, &["config", "validate"]).status.success());
    assert!(run(&temp, &["config", "reset"]).status.success());

    let backup = fs::read_dir(temp.path())
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path();
            (path.extension().is_some_and(|e| e == "bak")).then_some(path)
        })
        .next()
        .unwrap();

    assert_eq!(fs::read_to_string(backup).unwrap(), content);
    assert!(
        !fs::read_to_string(temp.path().join("config.toml"))
            .unwrap()
            .contains("directory =")
    );
}

#[test]
fn explicit_missing_config_is_error_two() {
    let temp = TempDir::new().unwrap();

    let output = run(&temp, &["preview", "-d", "."]);

    assert_eq!(output.status.code(), Some(2));
    assert!(!temp.path().join(".fortify").exists());
}

#[test]
fn cli_preview_apply_undo_and_help() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("config.toml"),
        "version=1\nignore=['config.toml']\n[mapping]\nDocuments=['pdf']",
    )
    .unwrap();
    fs::write(temp.path().join("report.pdf"), "data").unwrap();

    let preview = run(&temp, &["-d", "."]);

    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );

    assert!(String::from_utf8_lossy(&preview.stdout).contains("Documents"));
    assert!(!temp.path().join(".fortify").exists());
    assert!(run(&temp, &["apply", "-d", "."]).status.success());
    assert!(temp.path().join("Documents/report.pdf").exists());
    assert!(run(&temp, &["undo", "-d", "."]).status.success());
    assert!(temp.path().join("report.pdf").exists());
    assert!(run(&temp, &["--help"]).status.success());
}

#[test]
fn invalid_flags_and_ambiguous_schedule_are_error_two() {
    let temp = TempDir::new().unwrap();

    assert_eq!(
        run(
            &temp,
            &["schedule", "enable", "--every", "1h", "--daily", "09:00"]
        )
        .status
        .code(),
        Some(2)
    );

    assert_eq!(
        run(&temp, &["config", "init", "--no-sort"]).status.code(),
        Some(2)
    );

    assert_eq!(run(&temp, &["install"]).status.code(), Some(2));
}

#[test]
fn native_runner_logs_result_and_preserves_exit_status() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.toml");
    fs::write(
        &config,
        "version=1\nignore=['config.toml','run.log']\n[mapping]\nDocuments=['pdf']",
    )
    .unwrap();
    fs::write(temp.path().join("report.pdf"), "data").unwrap();
    let log = temp.path().join("run.log");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_fortify-runner"))
            .arg("--config")
            .arg(&config)
            .arg("--directory")
            .arg(temp.path())
            .arg("--log")
            .arg(&log)
            .output()
            .unwrap()
    };

    assert!(run().status.success());
    assert!(fs::read_to_string(&log).unwrap().contains("applied"));
    fs::write(&config, "broken").unwrap();

    assert_eq!(run().status.code(), Some(2));
    assert!(
        fs::read_to_string(&log)
            .unwrap()
            .contains("invalid configuration")
    );
}

#[test]
fn undo_with_explicit_target_does_not_need_configuration() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("config.toml"),
        "version=1\n[mapping]\nDocuments=['pdf']",
    )
    .unwrap();
    fs::write(temp.path().join("report.pdf"), "data").unwrap();

    assert!(run(&temp, &["apply", "-d", "."]).status.success());
    fs::remove_file(temp.path().join("config.toml")).unwrap();

    assert!(run(&temp, &["undo", "-d", "."]).status.success());
    assert!(temp.path().join("report.pdf").exists());
}

#[test]
fn encoded_scheduled_request_preserves_literal_environment_variable_names() {
    use fortify::schedule::RunnerRequest;

    let temp = TempDir::new().unwrap();
    let directory = temp.path().join("%TEMP% & Downloads");
    fs::create_dir(&directory).unwrap();
    let config = temp.path().join("%USERPROFILE%.toml");
    let log = temp.path().join("%TMP%.log");
    fs::write(&config, "version=1\n[mapping]\nDocuments=['pdf']").unwrap();
    fs::write(directory.join("report.pdf"), "data").unwrap();
    let request = RunnerRequest {
        version: 1,
        config,
        directory: directory.clone(),
        log: log.clone(),
    };

    let encoded = request.encode().unwrap();

    assert!(!encoded.contains('%'));
    assert_eq!(RunnerRequest::decode(&encoded).unwrap(), request);

    let result = Command::new(env!("CARGO_BIN_EXE_fortify-runner"))
        .args(["--request", &encoded])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    assert!(directory.join("Documents/report.pdf").exists());
    assert!(fs::read_to_string(log).unwrap().contains("applied"));
}
