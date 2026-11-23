//! Process-wide gate consulted immediately before a tool body runs.
//!
//! The emergency-stop authority lives in the runtime, but tools are executed
//! from several crates (the pipeline tool, skill aliases, wrappers). Those
//! crates cannot depend on the runtime, so the runtime installs its decision
//! function here once per process and every execution path that reaches a
//! tool body asks the same gate.
//!
//! The gate is installed for the lifetime of the process and cannot be
//! replaced: a config reload or a new daemon generation must not swap the
//! authority and with it the live stop state.

use std::sync::OnceLock;

/// The tool call was refused by the installed gate.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{reason}")]
pub struct DispatchDenied {
    reason: String,
}

impl DispatchDenied {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Decision function for tool execution.
///
/// `check` must be cheap and non-blocking: it runs on every tool call.
pub trait DispatchGate: Send + Sync {
    /// Decide whether the tool with this canonical name may start now.
    fn check_tool(&self, tool_name: &str) -> Result<(), DispatchDenied>;

    /// Decide whether work that is not a named tool (a scheduled shell job,
    /// an SOP capability step) may start now. `what` is a short label for
    /// the refusal message.
    fn check_work(&self, what: &str) -> Result<(), DispatchDenied>;
}

static GATE: OnceLock<Box<dyn DispatchGate>> = OnceLock::new();

/// Install the process-wide gate. Returns `false` when one is already
/// installed; the installed gate is kept.
pub fn install(gate: Box<dyn DispatchGate>) -> bool {
    GATE.set(gate).is_ok()
}

tokio::task_local! {
    static SCOPED_GATE: std::sync::Arc<dyn DispatchGate>;
}

/// Run `future` with an additional gate that applies only to work performed
/// inside it (on the same task; spawned tasks do not inherit it). The scoped
/// gate is consulted together with the process-wide one and both must allow,
/// so a scope can only add restrictions. Intended for tests that must not
/// install a process-wide gate.
pub async fn with_scoped_gate<F: std::future::Future>(
    gate: std::sync::Arc<dyn DispatchGate>,
    future: F,
) -> F::Output {
    SCOPED_GATE.scope(gate, future).await
}

/// Ask the installed gate about a named tool. With no gate installed the
/// call is allowed: the owner of the process installs a gate before it starts
/// any worker whenever its policy requires one.
pub fn check_tool(tool_name: &str) -> Result<(), DispatchDenied> {
    if let Ok(scoped) = SCOPED_GATE.try_with(std::sync::Arc::clone) {
        scoped.check_tool(tool_name)?;
    }
    match GATE.get() {
        Some(gate) => gate.check_tool(tool_name),
        None => Ok(()),
    }
}

/// Ask the installed gate about non-tool work. See [`check_tool`].
pub fn check_work(what: &str) -> Result<(), DispatchDenied> {
    if let Ok(scoped) = SCOPED_GATE.try_with(std::sync::Arc::clone) {
        scoped.check_work(what)?;
    }
    match GATE.get() {
        Some(gate) => gate.check_work(what),
        None => Ok(()),
    }
}
