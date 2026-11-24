//! `zeroclaw estop` against a real daemon in another process: the scenario the
//! emergency stop exists for. Unix only: the daemon's local socket is a Unix
//! socket here.
#![cfg(unix)]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_config(dir: &Path, port: u16, state_file: &Path, require_otp: bool) {
    write_config_with_otp(dir, port, state_file, require_otp, false);
}

/// `otp_enabled` turns on `security.otp`, which makes the daemon enroll a
/// secret in the config directory when it starts.
fn write_config_with_otp(
    dir: &Path,
    port: u16,
    state_file: &Path,
    require_otp: bool,
    otp_enabled: bool,
) {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "schema_version = 4\nlocale = \"en\"\n\n[gateway]\nport = {port}\nrequire_pairing = false\n\n[security.otp]\nenabled = {otp_enabled}\n\n[security.estop]\nenabled = true\nrequire_otp_to_resume = {require_otp}\nstate_file = '{}'\n",
            state_file.display()
        ),
    )
    .unwrap();
}

fn zeroclaw_command(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    command
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("RUST_LOG", "off")
        .env_remove("ZEROCLAW_CONFIG_DIR")
        .env_remove("ZEROCLAW_DATA_DIR")
        .env_remove("ZEROCLAW_WORKSPACE")
        .env_remove("ZEROCLAW_SOCKET")
        .arg("--config-dir")
        .arg(dir);
    command
}

fn spawn_daemon(dir: &Path, port: u16, tag: &str) -> KillOnDrop {
    let stdout = std::fs::File::create(dir.join(format!("{tag}.stdout"))).unwrap();
    let stderr = std::fs::File::create(dir.join(format!("{tag}.stderr"))).unwrap();
    let child = zeroclaw_command(dir)
        .arg("daemon")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--allow-degraded-security")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();
    KillOnDrop(child)
}

fn log(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

const RPC_INITIALIZE: &str =
    r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol_version":1},"id":1}"#;

fn rpc_initialize_ok(socket: &Path) -> bool {
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket) else {
        return false;
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .is_err()
        || writeln!(stream, "{RPC_INITIALIZE}").is_err()
    {
        return false;
    }
    let mut response = [0_u8; 4096];
    let Ok(read) = stream.read(&mut response) else {
        return false;
    };
    String::from_utf8_lossy(&response[..read]).contains("\"result\"")
}

/// One JSON-RPC request on a fresh local connection, after `initialize`; the
/// `error` object of the response, or `None` when it succeeded.
fn rpc_error(socket: &Path, method: &str, params: &str) -> Option<serde_json::Value> {
    use std::io::BufRead;
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    writeln!(stream, "{RPC_INITIALIZE}").unwrap();
    writeln!(
        stream,
        r#"{{"jsonrpc":"2.0","method":"{method}","params":{params},"id":2}}"#
    )
    .unwrap();
    let mut reader = std::io::BufReader::new(stream);
    loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "daemon closed the socket"
        );
        let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        if frame["id"] == 2 {
            return frame.get("error").cloned();
        }
    }
}

fn wait_until_ready(daemon: &mut KillOnDrop, dir: &Path, tag: &str) {
    let socket = dir.join("data").join("daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            panic!(
                "the daemon exited before it was ready: {status:?}\nstdout:\n{}\nstderr:\n{}",
                log(dir, &format!("{tag}.stdout")),
                log(dir, &format!("{tag}.stderr"))
            );
        }
        if rpc_initialize_ok(&socket) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "the daemon was not ready within 90 s\nstdout:\n{}\nstderr:\n{}",
        log(dir, &format!("{tag}.stdout")),
        log(dir, &format!("{tag}.stderr"))
    );
}

fn estop(dir: &Path, args: &[&str]) -> Output {
    zeroclaw_command(dir)
        .arg("estop")
        .args(args)
        .output()
        .expect("failed to run zeroclaw estop")
}

/// `zeroclaw estop <args>` with `input` on its standard input.
fn estop_with_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    let mut child = zeroclaw_command(dir)
        .arg("estop")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run zeroclaw estop");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_ok(output: &Output, what: &str) -> String {
    let stdout = stdout_of(output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{what} must succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

#[test]
fn estop_from_another_process_is_live_in_the_daemon_and_survives_its_restart() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    let port = free_port();
    write_config(dir.path(), port, &state_file, false);

    let mut daemon = spawn_daemon(dir.path(), port, "first");
    wait_until_ready(&mut daemon, dir.path(), "first");

    // Nothing is engaged yet.
    let status = assert_ok(&estop(dir.path(), &["status"]), "estop status");
    assert!(status.contains("kill_all:        false"), "{status}");

    // Engage from this separate CLI process: the daemon reports the stop.
    let engaged = assert_ok(&estop(dir.path(), &[]), "estop");
    assert!(engaged.contains("Estop engaged."), "{engaged}");
    assert!(engaged.contains("kill_all:        true"), "{engaged}");
    let status = assert_ok(&estop(dir.path(), &["status"]), "estop status after engage");
    assert!(status.contains("kill_all:        true"), "{status}");
    let on_disk = std::fs::read_to_string(&state_file).unwrap();
    assert!(on_disk.contains("\"kill_all\": true"), "{on_disk}");

    // Restart the daemon: the stop is still in force.
    drop(daemon);
    let mut daemon = spawn_daemon(dir.path(), port, "second");
    wait_until_ready(&mut daemon, dir.path(), "second");
    let status = assert_ok(
        &estop(dir.path(), &["status"]),
        "estop status after restart",
    );
    assert!(
        status.contains("kill_all:        true"),
        "an engaged stop must survive a daemon restart\n{status}"
    );

    // Resume releases it, and the release is also what a later status shows.
    let resumed = assert_ok(&estop(dir.path(), &["resume"]), "estop resume");
    assert!(resumed.contains("Estop resume completed."), "{resumed}");
    let status = assert_ok(&estop(dir.path(), &["status"]), "estop status after resume");
    assert!(status.contains("kill_all:        false"), "{status}");
}

#[test]
fn tool_freeze_names_only_the_requested_tool_and_a_misplaced_scope_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    let port = free_port();
    write_config(dir.path(), port, &state_file, false);

    let mut daemon = spawn_daemon(dir.path(), port, "first");
    wait_until_ready(&mut daemon, dir.path(), "first");

    // A scope flag without its level is refused and the daemon is untouched.
    let refused = estop(dir.path(), &["--tool", "shell"]);
    assert!(!refused.status.success());
    let status = assert_ok(&estop(dir.path(), &["status"]), "estop status");
    assert!(status.contains("kill_all:        false"), "{status}");
    assert!(!status.contains("shell"), "{status}");

    let frozen = assert_ok(
        &estop(dir.path(), &["--level", "tool-freeze", "--tool", "shell"]),
        "estop --level tool-freeze --tool shell",
    );
    assert!(frozen.contains("tool_freeze:    shell"), "{frozen}");
    assert!(
        frozen.contains("kill_all:        false"),
        "freezing one tool must not engage kill-all\n{frozen}"
    );

    let resumed = assert_ok(
        &estop(dir.path(), &["resume", "--tool", "shell"]),
        "estop resume --tool shell",
    );
    assert!(resumed.contains("tool_freeze:    (none)"), "{resumed}");
}

#[test]
fn the_daemon_refuses_to_start_when_otp_is_required_and_no_secret_is_enrolled() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    let port = free_port();
    write_config(dir.path(), port, &state_file, true);

    let mut daemon = spawn_daemon(dir.path(), port, "first");
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon kept running with estop OTP required and no enrolled secret\nstdout:\n{}\nstderr:\n{}",
            log(dir.path(), "first.stdout"),
            log(dir.path(), "first.stderr")
        );
        std::thread::sleep(Duration::from_millis(100));
    };

    let stderr = log(dir.path(), "first.stderr");
    assert!(!status.success(), "exit={status:?}\n{stderr}");
    assert!(
        stderr.contains("OTP secret is enrolled"),
        "the refusal must say why\n{stderr}"
    );
    assert!(
        !dir.path().join("otp-secret").exists(),
        "the daemon must not mint an OTP secret nobody holds"
    );
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Codes an authenticator could show now, one step either side included: each
/// is accepted by the daemon once.
struct Authenticator {
    validator: zeroclaw_runtime::security::OtpValidator,
    spent: Vec<String>,
}

impl Authenticator {
    fn enrolled_in(dir: &Path) -> Self {
        let config = zeroclaw_config::schema::OtpConfig::default();
        let store = zeroclaw_runtime::security::SecretStore::new(dir, true);
        let validator =
            zeroclaw_runtime::security::OtpValidator::from_existing(&config, dir, &store)
                .expect("the daemon enrolled a secret when it started");
        Self {
            validator,
            spent: Vec::new(),
        }
    }

    /// A valid code that no earlier call returned.
    fn fresh_code(&mut self) -> String {
        let now = unix_now();
        for timestamp in [now, now + 30, now.saturating_sub(30)] {
            let code = self.validator.code_for_timestamp(timestamp);
            if !self.spent.contains(&code) {
                self.spent.push(code.clone());
                return code;
            }
        }
        panic!("no unused valid code left in the accepted window");
    }

    /// Six digits that are not valid in the accepted window.
    fn wrong_code(&self) -> String {
        let now = unix_now();
        let valid: Vec<String> = [
            now.saturating_sub(60),
            now.saturating_sub(30),
            now,
            now + 30,
            now + 60,
        ]
        .iter()
        .map(|timestamp| self.validator.code_for_timestamp(*timestamp))
        .collect();
        (0..1000)
            .map(|n| format!("{n:06}"))
            .find(|candidate| !valid.contains(candidate))
            .unwrap()
    }
}

#[test]
fn resume_with_otp_asks_for_a_code_refuses_wrong_and_replayed_ones_and_locks_after_failures() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    let port = free_port();
    write_config_with_otp(dir.path(), port, &state_file, true, true);

    let mut daemon = spawn_daemon(dir.path(), port, "first");
    wait_until_ready(&mut daemon, dir.path(), "first");
    let mut authenticator = Authenticator::enrolled_in(dir.path());
    let engaged =
        |dir: &Path| stdout_of(&estop(dir, &["status"])).contains("kill_all:        true");

    assert_ok(&estop(dir.path(), &[]), "estop");
    assert!(engaged(dir.path()));

    // No code: refused, and as often as you like, because it is not a guess.
    for _ in 0..4 {
        let output = estop(dir.path(), &["resume"]);
        assert!(!output.status.success());
        assert!(
            stderr_of(&output).contains("OTP code is required"),
            "{}",
            stderr_of(&output)
        );
    }
    assert!(engaged(dir.path()), "a refused resume changes nothing");

    // Over the wire, the missing code is told apart from a wrong one by data
    // a client can act on; the CLI reads it to know when to prompt.
    let socket = dir.path().join("data").join("daemon.sock");
    let missing = rpc_error(&socket, "estop/resume", r#"{"kill_all":true}"#)
        .expect("a resume with no code is an error");
    assert_eq!(missing["data"]["reason"], "otp_required", "{missing}");
    let wrong_over_rpc = rpc_error(
        &socket,
        "estop/resume",
        &format!(
            r#"{{"kill_all":true,"otp":"{}"}}"#,
            authenticator.wrong_code()
        ),
    )
    .expect("a wrong code is an error");
    assert!(wrong_over_rpc.get("data").is_none(), "{wrong_over_rpc}");
    assert!(engaged(dir.path()));

    // A wrong code is refused.
    let wrong = estop(
        dir.path(),
        &["resume", "--otp", &authenticator.wrong_code()],
    );
    assert!(!wrong.status.success());
    assert!(
        stderr_of(&wrong).contains("Invalid OTP code"),
        "{}",
        stderr_of(&wrong)
    );
    assert!(engaged(dir.path()));

    // After four requests with no code and one wrong code, a right one still works.
    let code = authenticator.fresh_code();
    let resumed = assert_ok(
        &estop(dir.path(), &["resume", "--otp", &code]),
        "estop resume --otp <code>",
    );
    assert!(resumed.contains("Estop resume completed."), "{resumed}");
    assert!(!engaged(dir.path()));

    // The same code cannot resume a second stop.
    assert_ok(&estop(dir.path(), &[]), "estop");
    let replay = estop(dir.path(), &["resume", "--otp", &code]);
    assert!(!replay.status.success());
    assert!(
        stderr_of(&replay).contains("Invalid OTP code"),
        "{}",
        stderr_of(&replay)
    );
    assert!(engaged(dir.path()));

    // A fresh code read from standard input (`--otp -`) works.
    let code = authenticator.fresh_code();
    let output = estop_with_stdin(dir.path(), &["resume", "--otp", "-"], &format!("{code}\n"));
    let resumed = assert_ok(&output, "estop resume --otp - (code on stdin)");
    assert!(resumed.contains("Estop resume completed."), "{resumed}");
    assert!(!engaged(dir.path()));

    // Three wrong codes in a row lock resume, even for a code that is right.
    assert_ok(&estop(dir.path(), &[]), "estop");
    for _ in 0..3 {
        let wrong = estop(
            dir.path(),
            &["resume", "--otp", &authenticator.wrong_code()],
        );
        assert!(!wrong.status.success());
    }
    let locked = estop(
        dir.path(),
        &["resume", "--otp", &authenticator.fresh_code()],
    );
    assert!(!locked.status.success());
    assert!(
        stderr_of(&locked).contains("Too many failed OTP attempts"),
        "{}",
        stderr_of(&locked)
    );
    assert!(
        engaged(dir.path()),
        "a locked resume leaves the stop in place"
    );
}
