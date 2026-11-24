use std::path::Path;
use std::process::{Command, Output};

const STATE_BEFORE: &str = "{\n  \"kill_all\": false,\n  \"network_kill\": false,\n  \"blocked_domains\": [],\n  \"frozen_tools\": [],\n  \"updated_at\": null\n}\n";

fn write_config(config_dir: &Path, state_file: &Path, enabled: bool) {
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "schema_version = 4\nlocale = \"en\"\n\n[security.estop]\nenabled = {enabled}\nrequire_otp_to_resume = false\nstate_file = '{}'\n",
            state_file.display()
        ),
    )
    .unwrap();
}

fn run_estop(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("RUST_LOG", "off")
        .env_remove("ZEROCLAW_CONFIG_DIR")
        .env_remove("ZEROCLAW_DATA_DIR")
        .env_remove("ZEROCLAW_WORKSPACE")
        .env_remove("ZEROCLAW_SOCKET")
        .arg("--config-dir")
        .arg(config_dir)
        .arg("estop")
        .args(args)
        .output()
        .expect("failed to run zeroclaw estop")
}

fn assert_refused_without_daemon(output: &Output, args: &[&str]) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "`zeroclaw estop {args:?}` must not succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("`zeroclaw estop` needs the running ZeroClaw daemon"),
        "`zeroclaw estop {args:?}` must say a running daemon is required\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("Estop engaged") && !stdout.contains("Estop resume completed"),
        "`zeroclaw estop {args:?}` must not report a stop\nstdout:\n{stdout}"
    );
}

/// A scope flag (`--tool`, `--domain`) without the `--level` that takes it is a
/// usage error, reported before any daemon is contacted. Each entry carries the
/// corrected form the message must suggest.
const MISMATCHED_SCOPE_FORMS: &[(&[&str], &str)] = &[
    (&["--tool", "shell"], "--level tool-freeze"),
    (&["--domain", "x.com"], "--level domain-block"),
    (
        &["--level", "kill-all", "--tool", "shell"],
        "--level tool-freeze",
    ),
    (
        &["--level", "kill-all", "--domain", "x.com"],
        "--level domain-block",
    ),
    (
        &["--level", "network-kill", "--domain", "x.com"],
        "--level domain-block",
    ),
];

fn assert_usage_error_before_daemon(output: &Output, args: &[&str], suggestion: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "`zeroclaw estop {args:?}` must not succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(suggestion),
        "`zeroclaw estop {args:?}` must suggest `{suggestion}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("needs the running ZeroClaw daemon"),
        "`zeroclaw estop {args:?}` is a usage error and must fail before a daemon is contacted\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("Estop engaged"),
        "`zeroclaw estop {args:?}` must not report a stop\nstdout:\n{stdout}"
    );
}

const FORMS: &[&[&str]] = &[
    &[],
    &["--level", "kill-all"],
    &["--level", "network-kill"],
    &["--level", "domain-block", "--domain", "*.example.com"],
    &["--level", "tool-freeze", "--tool", "shell"],
    &["status"],
    &["resume"],
    &["resume", "--network"],
    &["resume", "--domain", "*.example.com"],
    &["resume", "--tool", "shell"],
    &["resume", "--otp", "123456"],
    &["--level", "domain-block"],
    &["resume", "--network", "--tool", "shell"],
];

#[test]
fn estop_without_a_daemon_refuses_every_form_and_leaves_the_state_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    write_config(dir.path(), &state_file, true);
    std::fs::write(&state_file, STATE_BEFORE).unwrap();

    for args in FORMS {
        let output = run_estop(dir.path(), args);
        assert_refused_without_daemon(&output, args);
        assert_eq!(
            std::fs::read_to_string(&state_file).unwrap(),
            STATE_BEFORE,
            "`zeroclaw estop {args:?}` changed the state file"
        );
    }
}

#[test]
fn estop_without_a_daemon_does_not_create_a_state_file_whether_enabled_or_not() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    write_config(dir.path(), &state_file, false);

    for args in FORMS {
        let output = run_estop(dir.path(), args);
        assert_refused_without_daemon(&output, args);
        assert!(
            !state_file.exists(),
            "`zeroclaw estop {args:?}` created the state file"
        );
    }
}

#[test]
fn a_scope_flag_without_its_level_is_a_usage_error_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    write_config(dir.path(), &state_file, true);
    std::fs::write(&state_file, STATE_BEFORE).unwrap();

    for (args, suggestion) in MISMATCHED_SCOPE_FORMS {
        let output = run_estop(dir.path(), args);
        assert_usage_error_before_daemon(&output, args, suggestion);
        assert_eq!(
            std::fs::read_to_string(&state_file).unwrap(),
            STATE_BEFORE,
            "`zeroclaw estop {args:?}` changed the state file"
        );
    }

    let disabled = tempfile::tempdir().unwrap();
    let absent = disabled.path().join("estop-state.json");
    write_config(disabled.path(), &absent, false);
    for (args, suggestion) in MISMATCHED_SCOPE_FORMS {
        let output = run_estop(disabled.path(), args);
        assert_usage_error_before_daemon(&output, args, suggestion);
        assert!(
            !absent.exists(),
            "`zeroclaw estop {args:?}` created the state file"
        );
    }
}

/// Run `zeroclaw <args>` and kill it if it is still running after `limit`, so
/// a command that should have refused to start cannot hang the test.
fn run_command_with_timeout(
    config_dir: &Path,
    args: &[&str],
    limit: std::time::Duration,
) -> Output {
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("RUST_LOG", "off")
        .env_remove("ZEROCLAW_CONFIG_DIR")
        .env_remove("ZEROCLAW_DATA_DIR")
        .env_remove("ZEROCLAW_WORKSPACE")
        .env_remove("ZEROCLAW_SOCKET")
        .arg("--config-dir")
        .arg(config_dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run zeroclaw");
    let deadline = std::time::Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "`zeroclaw {args:?}` was still running after {limit:?}; it should have refused to start\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn commands_that_run_tools_outside_the_daemon_refuse_to_start_while_estop_is_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    write_config(dir.path(), &state_file, true);

    for (args, shown) in [
        (&["agent", "--agent", "default"][..], "zeroclaw agent"),
        (&["gateway"][..], "zeroclaw gateway"),
        (&["gateway", "start"][..], "zeroclaw gateway"),
        (&["channel", "start"][..], "zeroclaw channel start"),
    ] {
        let output = run_command_with_timeout(dir.path(), args, std::time::Duration::from_secs(60));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "`zeroclaw {args:?}` must refuse to start\nstderr:\n{stderr}"
        );
        assert!(
            stderr.contains(&format!("`{shown}` runs tools in its own process")),
            "`zeroclaw {args:?}` must say why it refused\nstderr:\n{stderr}"
        );
        assert!(
            stderr.contains("security.estop.enabled"),
            "`zeroclaw {args:?}` must name the setting\nstderr:\n{stderr}"
        );
        assert!(
            !state_file.exists(),
            "`zeroclaw {args:?}` must not create the state file"
        );
    }
}
