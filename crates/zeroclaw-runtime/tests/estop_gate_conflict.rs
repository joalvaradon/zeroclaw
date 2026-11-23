//! A process whose dispatch-gate slot is already taken cannot also own an
//! emergency stop: the stop would be reported and never enforced.
//!
//! The gate is process-global, so this binary holds exactly one test.

use zeroclaw_api::dispatch_gate::{DispatchDenied, DispatchGate};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::security::install_for_config;

struct OtherGate;

impl DispatchGate for OtherGate {
    fn check_tool(&self, _tool_name: &str) -> Result<(), DispatchDenied> {
        Ok(())
    }

    fn check_work(&self, _what: &str) -> Result<(), DispatchDenied> {
        Ok(())
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

#[test]
fn an_estop_cannot_start_when_another_gate_already_holds_the_slot() {
    assert!(zeroclaw_api::dispatch_gate::install(Box::new(OtherGate)));

    let dir = tempfile::tempdir().unwrap();
    let error = install_for_config(&enabled_config(dir.path()))
        .expect_err("a stop that nothing enforces must not be reported as installed");
    assert!(
        error.to_string().contains("another dispatch gate"),
        "{error:#}"
    );
}
