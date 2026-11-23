//! The emergency-stop authority and the process-wide dispatch gate together.
//!
//! The authority and the gate are process-global, so this binary holds exactly
//! one test and nothing else in it touches them. It does not cover the CLI or
//! the RPC leg: those need a running daemon in another process.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use zeroclaw_api::tool::{Tool, ToolExecutionContext, ToolResult};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::security::{EstopAuthority, EstopLevel, ResumeSelector, install_for_config};

struct Probe {
    calls: Arc<AtomicUsize>,
}

zeroclaw_api::mock_tool_attribution!(Probe);

#[async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        "probe_tool"
    }
    fn description(&self) -> &str {
        "Counts how many times its body ran"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::ok("ran"))
    }
}

#[allow(clippy::field_reassign_with_default)]
fn enabled_config(dir: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.config_path = dir.join("config.toml");
    config.security.estop.enabled = true;
    config.security.estop.require_otp_to_resume = false;
    config.security.estop.state_file = dir.join("estop-state.json").display().to_string();
    config
}

#[tokio::test]
async fn engage_denies_tool_calls_through_reload_and_restart_until_resume() {
    let dir = tempfile::tempdir().unwrap();
    let config = enabled_config(dir.path());
    let authority = install_for_config(&config)
        .unwrap()
        .expect("estop is enabled, so the process gets an authority");

    let calls = Arc::new(AtomicUsize::new(0));
    let probe = Probe {
        calls: Arc::clone(&calls),
    };
    let context = ToolExecutionContext::default();
    let args = serde_json::json!({});

    // Nothing engaged: the tool body runs.
    assert!(
        probe
            .execute_with_context(args.clone(), &context)
            .await
            .unwrap()
            .success
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(zeroclaw_api::dispatch_gate::check_work("scheduled job").is_ok());

    // Engage: the next call is refused before the body runs.
    let engaged = authority.engage(EstopLevel::KillAll).unwrap();
    assert!(engaged.durable, "{:?}", engaged.persist_error);
    let denied = probe
        .execute_with_context(args.clone(), &context)
        .await
        .expect_err("an engaged stop must refuse the call");
    assert!(
        denied.to_string().contains("emergency stop is engaged"),
        "{denied}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the body must not run");
    assert!(zeroclaw_api::dispatch_gate::check_work("scheduled job").is_err());

    // A daemon reload re-enters with a different config: the decision made at
    // first start stands, and the live state is not rebuilt from the file.
    let mut reloaded = config.clone();
    reloaded.security.estop.enabled = false;
    let again = install_for_config(&reloaded)
        .unwrap()
        .expect("the first decision is fixed for the life of the process");
    assert!(Arc::ptr_eq(&authority, &again));
    assert!(
        probe
            .execute_with_context(args.clone(), &context)
            .await
            .is_err()
    );

    // Restart: a new authority over the same state file starts engaged, with a
    // revision above the one it last handed out.
    let restarted = EstopAuthority::new(&config.security.estop, dir.path(), None).unwrap();
    assert!(restarted.status().state.kill_all);
    assert!(restarted.status().revision > engaged.revision);

    // Resume: the tool runs again.
    authority.resume(ResumeSelector::KillAll, None).unwrap();
    assert!(
        probe
            .execute_with_context(args, &context)
            .await
            .unwrap()
            .success
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
