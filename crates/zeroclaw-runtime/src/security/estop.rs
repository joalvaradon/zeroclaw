//! Emergency-stop authority.
//!
//! One [`EstopAuthority`] exists per process. It owns the live stop state and
//! a monotonic revision; every `engage`/`resume` is one transaction under a
//! single lock (read, authorize, mutate, commit). The tool-dispatch gate reads
//! the same in-memory state through [`zeroclaw_api::dispatch_gate`], so there
//! is no cache and no acknowledgement barrier: a call whose gate runs after an
//! `engage` returned observes it.
//!
//! The state file is a recovery record. It is read once when the authority is
//! created and never again; its contents or presence do not decide live
//! enforcement.
//!
//! Enforced today: `kill_all` and `frozen_tools`. `network_kill` and
//! `blocked_domains` are not enforced by any path; they are accepted only as
//! legacy fields in an existing state file, reported as such, and can be
//! cleared with `resume`.

use crate::security::otp::OtpValidator;
use anyhow::{Context, Result};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroclaw_api::dispatch_gate::{DispatchDenied, DispatchGate};
use zeroclaw_config::schema::EstopConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EstopLevel {
    KillAll,
    /// Not enforced by any runtime path; `engage` refuses it.
    NetworkKill,
    /// Not enforced by any runtime path; `engage` refuses it.
    DomainBlock(Vec<String>),
    ToolFreeze(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeSelector {
    KillAll,
    Network,
    Domains(Vec<String>),
    Tools(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct EstopState {
    #[serde(default)]
    pub kill_all: bool,
    /// Legacy field: no runtime path enforces it.
    #[serde(default)]
    pub network_kill: bool,
    /// Legacy field: no runtime path enforces it.
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    #[serde(default)]
    pub frozen_tools: Vec<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Monotonic across restarts: a restart resumes from the persisted value
    /// plus one, so a revision is never reused for different contents.
    #[serde(default)]
    pub revision: u64,
}

impl EstopState {
    pub fn fail_closed() -> Self {
        Self {
            kill_all: true,
            network_kill: false,
            blocked_domains: Vec::new(),
            frozen_tools: Vec::new(),
            updated_at: Some(now_rfc3339()),
            revision: 0,
        }
    }

    pub fn is_engaged(&self) -> bool {
        self.kill_all
            || self.network_kill
            || !self.blocked_domains.is_empty()
            || !self.frozen_tools.is_empty()
    }

    /// Whether the state holds legacy dimensions that nothing enforces.
    pub fn has_unenforced(&self) -> bool {
        self.network_kill || !self.blocked_domains.is_empty()
    }

    fn normalize(&mut self) {
        self.blocked_domains = dedup_sort(&self.blocked_domains);
        self.frozen_tools = dedup_sort(&self.frozen_tools);
    }
}

/// A committed state with its revision and whether it reached the state file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EstopSnapshot {
    pub state: EstopState,
    pub revision: u64,
    /// `false` when the state is live in memory but the state file could not
    /// be written: a restart would lose it.
    pub durable: bool,
    /// Why the state file write failed, when `durable` is `false`.
    pub persist_error: Option<String>,
}

struct Inner {
    state: EstopState,
    durable: bool,
    persist_error: Option<String>,
}

impl Inner {
    fn snapshot(&self) -> EstopSnapshot {
        EstopSnapshot {
            revision: self.state.revision,
            state: self.state.clone(),
            durable: self.durable,
            persist_error: self.persist_error.clone(),
        }
    }
}

/// The operator is not authorized to resume (missing, wrong, replayed or
/// rate-limited OTP). A distinct type so callers classify the refusal by type
/// and not by message text.
#[derive(Debug)]
pub struct ResumeDenied(pub String);

impl std::fmt::Display for ResumeDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ResumeDenied {}

/// The resume was authorized but the state file could not be written, so the
/// stop is still in place. Not a refusal of the request: the caller reports a
/// server fault, and tells the operator whether the one-time code was spent.
#[derive(Debug)]
pub struct ResumeNotSaved(pub String);

impl std::fmt::Display for ResumeNotSaved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ResumeNotSaved {}

/// A resume that needs a one-time code and was sent none: the one refusal an
/// operator fixes by supplying a code, so the RPC handler tells the client to
/// ask for one. It does not count against the OTP failure limit.
#[derive(Debug)]
pub struct OtpRequired(pub String);

impl std::fmt::Display for OtpRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OtpRequired {}

pub struct EstopAuthority {
    /// Serializes `engage`/`resume` transactions across their file I/O.
    txn: Mutex<()>,
    /// Live state. Held only briefly: readers are the dispatch gate.
    inner: RwLock<Inner>,
    state_path: PathBuf,
    require_otp_to_resume: bool,
    otp: Option<OtpValidator>,
}

impl std::fmt::Debug for EstopAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EstopAuthority")
            .field("state_path", &self.state_path)
            .field("require_otp_to_resume", &self.require_otp_to_resume)
            .finish_non_exhaustive()
    }
}

impl EstopAuthority {
    /// Create the authority and read the recovery record once.
    ///
    /// A missing file is "never engaged". Any other read or parse failure
    /// starts in `kill_all`. When `require_otp_to_resume` is set a validator
    /// must be supplied: the authority never mints a secret.
    pub fn new(config: &EstopConfig, config_dir: &Path, otp: Option<OtpValidator>) -> Result<Self> {
        if config.require_otp_to_resume && otp.is_none() {
            anyhow::bail!(
                "security.estop.require_otp_to_resume is true but no OTP secret is enrolled; \
                 enable security.otp and enroll a secret, or set require_otp_to_resume to false"
            );
        }

        let state_path = resolve_state_file_path(config_dir, &config.state_file);
        let mut fail_closed = false;
        let mut state = match fs::read_to_string(&state_path) {
            Ok(raw) => match serde_json::from_str::<EstopState>(&raw) {
                Ok(parsed) => parsed,
                Err(error) => {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"path": state_path.display().to_string(), "error": format!("{}", error)})), "Failed to parse estop state file; entering fail-closed mode: ");
                    fail_closed = true;
                    EstopState::fail_closed()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => EstopState::default(),
            Err(error) => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"path": state_path.display().to_string(), "error": format!("{}", error)})), "Failed to read estop state file; entering fail-closed mode: ");
                fail_closed = true;
                EstopState::fail_closed()
            }
        };
        state.normalize();
        // Never reuse a revision that an earlier process may have handed out.
        state.revision = state.revision.saturating_add(1);

        let mut durable = true;
        let mut persist_error = None;
        if fail_closed && let Err(error) = persist(&state_path, &state) {
            durable = false;
            persist_error = Some(format!("{error:#}"));
        }

        Ok(Self {
            txn: Mutex::new(()),
            inner: RwLock::new(Inner {
                state,
                durable,
                persist_error,
            }),
            state_path,
            require_otp_to_resume: config.require_otp_to_resume,
            otp,
        })
    }

    pub fn status(&self) -> EstopSnapshot {
        self.inner.read().snapshot()
    }

    /// Engage a level. The new state is live in memory before the state file
    /// is written (the safe direction); a failed write is reported in the
    /// returned snapshot (`durable == false`), not hidden.
    pub fn engage(&self, level: EstopLevel) -> Result<EstopSnapshot> {
        // Validate before taking the lock: a refused request changes nothing.
        let mut next_tools = Vec::new();
        let mut kill_all = false;
        match level {
            EstopLevel::KillAll => kill_all = true,
            EstopLevel::NetworkKill | EstopLevel::DomainBlock(_) => {
                anyhow::bail!(
                    "network-kill and domain-block are not enforced by any runtime path and cannot be engaged"
                );
            }
            EstopLevel::ToolFreeze(tools) => {
                if tools.is_empty() {
                    anyhow::bail!("tool-freeze requires at least one tool name");
                }
                for tool in tools {
                    next_tools.push(normalize_tool_name(&tool)?);
                }
            }
        }

        // One transaction at a time; the state lock below is held only for
        // the in-memory publish, never across file I/O, so tool dispatch is
        // not blocked by a slow disk.
        let _txn = self.txn.lock();
        let mut next = self.inner.read().state.clone();
        if kill_all {
            next.kill_all = true;
        }
        next.frozen_tools.extend(next_tools);
        next.updated_at = Some(now_rfc3339());
        next.revision = next.revision.saturating_add(1);
        next.normalize();

        // Publish first: from this point every gate check observes the stop.
        {
            let mut inner = self.inner.write();
            inner.state = next.clone();
            inner.durable = false;
            inner.persist_error = Some("not yet written to the state file".to_string());
        }

        let outcome = persist(&self.state_path, &next);
        let mut inner = self.inner.write();
        match outcome {
            Ok(()) => {
                inner.durable = true;
                inner.persist_error = None;
            }
            Err(error) => {
                inner.durable = false;
                inner.persist_error = Some(format!("{error:#}"));
            }
        }
        Ok(inner.snapshot())
    }

    /// Resume a selector. Authorization, mutation and commit happen under one
    /// lock. The new state is written to the state file before it is
    /// published in memory, so a failed write leaves the stop in place.
    pub fn resume(
        &self,
        selector: ResumeSelector,
        otp_code: Option<&str>,
    ) -> Result<EstopSnapshot> {
        self.resume_many(vec![selector], otp_code)
    }

    /// Resume several selectors in one transaction: one authorization, one
    /// revision, one state-file write.
    pub fn resume_many(
        &self,
        selectors: Vec<ResumeSelector>,
        otp_code: Option<&str>,
    ) -> Result<EstopSnapshot> {
        // Validate every selector before authorizing: a malformed request must
        // not consume a one-time code.
        let mut prepared = Vec::with_capacity(selectors.len());
        for selector in selectors {
            prepared.push(match selector {
                ResumeSelector::Tools(tools) => ResumeSelector::Tools(
                    tools
                        .iter()
                        .map(|tool| normalize_tool_name(tool))
                        .collect::<Result<Vec<_>>>()?,
                ),
                ResumeSelector::Domains(domains) => ResumeSelector::Domains(
                    domains
                        .iter()
                        .map(|domain| domain.trim().to_ascii_lowercase())
                        .collect(),
                ),
                other => other,
            });
        }

        let _txn = self.txn.lock();
        self.authorize_resume(otp_code)?;

        let mut next = self.inner.read().state.clone();
        for selector in prepared {
            match selector {
                ResumeSelector::KillAll => next.kill_all = false,
                ResumeSelector::Network => next.network_kill = false,
                ResumeSelector::Domains(domains) => {
                    next.blocked_domains
                        .retain(|existing| !domains.iter().any(|target| target == existing));
                }
                ResumeSelector::Tools(tools) => {
                    next.frozen_tools
                        .retain(|existing| !tools.iter().any(|target| target == existing));
                }
            }
        }
        next.updated_at = Some(now_rfc3339());
        next.revision = next.revision.saturating_add(1);
        next.normalize();

        // Persist before publishing: a failed write leaves the stop in place.
        // The state lock is not held across the write.
        if let Err(error) = persist(&self.state_path, &next) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.state_path.display().to_string(),
                        "error": format!("{error:#}"),
                    })),
                "estop resume authorized but the state file could not be written"
            );
            let spent = if self.require_otp_to_resume {
                "; the one-time code was used, so the next attempt needs a new one"
            } else {
                ""
            };
            return Err(anyhow::Error::new(ResumeNotSaved(format!(
                "the emergency-stop state file could not be written, so the stop is still in place{spent}"
            ))));
        }
        let mut inner = self.inner.write();
        inner.state = next;
        inner.durable = true;
        inner.persist_error = None;
        Ok(inner.snapshot())
    }

    /// Authorize a resume. The one-time code is consumed before the state
    /// file is written: if the write then fails the stop stays in place and
    /// the operator needs the next code.
    fn authorize_resume(&self, otp_code: Option<&str>) -> Result<()> {
        if !self.require_otp_to_resume {
            return Ok(());
        }
        let denied = |message: &str| anyhow::Error::new(ResumeDenied(message.to_string()));
        let code = otp_code
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::Error::new(OtpRequired(
                    "OTP code is required to resume estop state".to_string(),
                ))
            })?;
        let validator = self.otp.as_ref().ok_or_else(|| {
            denied("OTP validator is required to resume estop state with OTP enabled")
        })?;
        match validator.validate_single_use(code) {
            Ok(true) => Ok(()),
            Ok(false) => Err(denied("Invalid OTP code; estop resume denied")),
            Err(error) => Err(denied(&format!("{error:#}"))),
        }
    }

    fn deny_reason(&self, tool: Option<&str>) -> Option<String> {
        let inner = self.inner.read();
        if inner.state.kill_all {
            return Some(format!(
                "emergency stop is engaged (kill-all, revision {})",
                inner.state.revision
            ));
        }
        if let Some(tool) = tool {
            let canonical = tool.trim().to_ascii_lowercase();
            if inner.state.frozen_tools.contains(&canonical) {
                return Some(format!(
                    "tool `{canonical}` is frozen by the emergency stop (revision {})",
                    inner.state.revision
                ));
            }
        }
        None
    }
}

impl DispatchGate for EstopAuthority {
    fn check_tool(&self, tool_name: &str) -> std::result::Result<(), DispatchDenied> {
        match self.deny_reason(Some(tool_name)) {
            Some(reason) => Err(DispatchDenied::new(reason)),
            None => Ok(()),
        }
    }

    fn check_work(&self, what: &str) -> std::result::Result<(), DispatchDenied> {
        match self.deny_reason(None) {
            Some(reason) => Err(DispatchDenied::new(format!("{what} refused: {reason}"))),
            None => Ok(()),
        }
    }
}

struct SharedGate(Arc<EstopAuthority>);

impl DispatchGate for SharedGate {
    fn check_tool(&self, tool_name: &str) -> std::result::Result<(), DispatchDenied> {
        self.0.check_tool(tool_name)
    }

    fn check_work(&self, what: &str) -> std::result::Result<(), DispatchDenied> {
        self.0.check_work(what)
    }
}

/// The process-wide decision: `Some(authority)` when estop was enabled the
/// first time the process looked, `None` when it was disabled. Set once.
static AUTHORITY: OnceLock<Option<Arc<EstopAuthority>>> = OnceLock::new();

/// The process-wide authority, if estop was enabled at first start.
pub fn global() -> Option<Arc<EstopAuthority>> {
    AUTHORITY.get().and_then(|decided| decided.clone())
}

/// Create the process-wide authority and install it as the dispatch gate.
fn install_global(
    config: &EstopConfig,
    config_dir: &Path,
    otp: Option<OtpValidator>,
) -> Result<Arc<EstopAuthority>> {
    let authority = Arc::new(EstopAuthority::new(config, config_dir, otp)?);
    let mut decided_here = false;
    let decided = AUTHORITY.get_or_init(|| {
        decided_here = true;
        Some(Arc::clone(&authority))
    });
    let installed = match decided {
        Some(existing) => Arc::clone(existing),
        None => authority,
    };
    // Only the call that decided the authority installs it. If another gate
    // already holds the slot, the stop would be reported and never enforced,
    // so the process must not go on.
    if decided_here
        && !zeroclaw_api::dispatch_gate::install(Box::new(SharedGate(Arc::clone(&installed))))
    {
        anyhow::bail!(
            "another dispatch gate is already installed in this process; \
             an emergency stop would be reported but not enforced"
        );
    }
    Ok(installed)
}

/// Decide, once per process, whether an emergency-stop authority exists, and
/// install it as the dispatch gate when `security.estop.enabled` is set.
///
/// The first call fixes the answer for the life of the process: later calls
/// (a daemon reload re-enters the daemon) return the same decision and ignore
/// their config. `enabled`, `state_file` and `require_otp_to_resume` therefore
/// change only on restart, and a reload can neither install an authority that
/// was not there nor rebuild the live state from the state file.
///
/// Fails when the authority cannot be created (for example
/// `require_otp_to_resume` without an enrolled OTP secret): a process that
/// cannot honor an enabled emergency stop must not start without one.
pub fn install_for_config(
    config: &zeroclaw_config::schema::Config,
) -> Result<Option<Arc<EstopAuthority>>> {
    if let Some(decided) = AUTHORITY.get() {
        return Ok(decided.clone());
    }
    if !config.security.estop.enabled {
        let decided = AUTHORITY.get_or_init(|| None);
        return Ok(decided.clone());
    }
    let config_dir = config
        .config_path
        .parent()
        .context("Config path must have a parent directory")?;
    let otp = if config.security.estop.require_otp_to_resume {
        let store = crate::security::SecretStore::new(config_dir, config.secrets.encrypt);
        Some(
            OtpValidator::from_existing(&config.security.otp, config_dir, &store).context(
                "security.estop.require_otp_to_resume is true, so the emergency stop needs an \
                 enrolled OTP secret: set security.otp.enabled = true and start any zeroclaw \
                 command once to enroll one, or set security.estop.require_otp_to_resume = false",
            )?,
        )
    } else {
        None
    };
    install_global(&config.security.estop, config_dir, otp).map(Some)
}

pub fn resolve_state_file_path(config_dir: &Path, state_file: &str) -> PathBuf {
    let expanded = shellexpand::tilde(state_file).into_owned();
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    }
}

/// Write the state atomically and durably: temp file, `fsync`, rename, then
/// `fsync` of the directory where the platform allows it.
fn persist(state_path: &Path, state: &EstopState) -> Result<()> {
    let parent = state_path.parent();
    if let Some(parent) = parent {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create estop state dir {}",
                parent.display().to_string()
            )
        })?;
    }

    let body = serde_json::to_string_pretty(state).context("Failed to serialize estop state")?;
    let temp_path = state_path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp_path).with_context(|| {
        format!(
            "Failed to create temporary estop state file {}",
            temp_path.display()
        )
    })?;
    let written = file
        .write_all(body.as_bytes())
        .and_then(|()| file.sync_all())
        .with_context(|| {
            format!(
                "Failed to write temporary estop state file {}",
                temp_path.display()
            )
        });
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    if let Err(error) = fs::rename(&temp_path, state_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error).with_context(|| {
            format!(
                "Failed to atomically replace estop state file {}",
                state_path.display()
            )
        });
    }

    #[cfg(unix)]
    if let Some(parent) = parent {
        fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .with_context(|| format!("Failed to sync estop state dir {}", parent.display()))?;
    }

    Ok(())
}

/// A scope list that the requested level does not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MisplacedScope {
    /// Tool names given with a level other than `tool-freeze`.
    Tools,
    /// Domain patterns given with a level other than `domain-block`.
    Domains,
}

/// Which scope list, if any, the requested engage level does not take: tool
/// names belong to `tool-freeze` and domain patterns to `domain-block`.
/// Silently dropping the list would engage a different stop than the one
/// asked for. The CLI and the daemon both ask this one question; an unknown
/// level is not judged here, the caller rejects it separately.
pub fn misplaced_scope(
    level: &str,
    tools: &[String],
    domains: &[String],
) -> Option<MisplacedScope> {
    if !matches!(
        level,
        "kill-all" | "network-kill" | "domain-block" | "tool-freeze"
    ) {
        return None;
    }
    if !tools.is_empty() && level != "tool-freeze" {
        return Some(MisplacedScope::Tools);
    }
    if !domains.is_empty() && level != "domain-block" {
        return Some(MisplacedScope::Domains);
    }
    None
}

/// Tool names are matched case-insensitively against the registry's canonical
/// names; the gate lowercases the called name the same way.
fn normalize_tool_name(raw: &str) -> Result<String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        anyhow::bail!("Tool name must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        anyhow::bail!("Tool name '{raw}' contains invalid characters");
    }
    Ok(value)
}

fn dedup_sort(values: &[String]) -> Vec<String> {
    let mut deduped = values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    deduped.sort_unstable();
    deduped.dedup();
    deduped
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecretStore;
    use tempfile::tempdir;
    use zeroclaw_config::schema::OtpConfig;

    fn estop_config(path: &Path) -> EstopConfig {
        EstopConfig {
            enabled: true,
            state_file: path.display().to_string(),
            require_otp_to_resume: false,
        }
    }

    fn otp_validator(dir: &Path) -> OtpValidator {
        let otp_cfg = OtpConfig {
            enabled: true,
            ..OtpConfig::default()
        };
        let store = SecretStore::new(dir, true);
        OtpValidator::from_config(&otp_cfg, dir, &store).unwrap().0
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    }

    #[test]
    fn engage_freezes_tools_and_resume_unfreezes() {
        let dir = tempdir().unwrap();
        let cfg = estop_config(&dir.path().join("estop-state.json"));
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();

        authority
            .engage(EstopLevel::ToolFreeze(vec!["Shell".into()]))
            .unwrap();
        assert!(authority.check_tool("shell").is_err());
        assert!(authority.check_tool("SHELL").is_err());
        assert!(authority.check_tool("file_read").is_ok());
        assert!(authority.check_work("cron shell job").is_ok());

        authority
            .resume(ResumeSelector::Tools(vec!["shell".into()]), None)
            .unwrap();
        assert!(authority.check_tool("shell").is_ok());
    }

    #[test]
    fn kill_all_denies_every_tool_and_non_tool_work() {
        let dir = tempdir().unwrap();
        let cfg = estop_config(&dir.path().join("estop-state.json"));
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();

        authority.engage(EstopLevel::KillAll).unwrap();
        assert!(authority.check_tool("anything").is_err());
        assert!(authority.check_work("cron shell job").is_err());

        authority.resume(ResumeSelector::KillAll, None).unwrap();
        assert!(authority.check_tool("anything").is_ok());
    }

    #[test]
    fn unenforced_levels_are_refused() {
        let dir = tempdir().unwrap();
        let cfg = estop_config(&dir.path().join("estop-state.json"));
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();

        assert!(authority.engage(EstopLevel::NetworkKill).is_err());
        assert!(
            authority
                .engage(EstopLevel::DomainBlock(vec!["*.chase.com".into()]))
                .is_err()
        );
        assert!(!authority.status().state.is_engaged());
        assert_eq!(authority.status().revision, 1);
    }

    #[test]
    fn revision_is_monotonic_and_survives_restart() {
        let dir = tempdir().unwrap();
        let cfg = estop_config(&dir.path().join("estop-state.json"));

        let first = EstopAuthority::new(&cfg, dir.path(), None).unwrap();
        let r0 = first.status().revision;
        let r1 = first.engage(EstopLevel::KillAll).unwrap().revision;
        let r2 = first
            .resume(ResumeSelector::KillAll, None)
            .unwrap()
            .revision;
        let r3 = first.engage(EstopLevel::KillAll).unwrap().revision;
        assert!(r0 < r1 && r1 < r2 && r2 < r3);
        drop(first);

        let second = EstopAuthority::new(&cfg, dir.path(), None).unwrap();
        assert!(second.status().state.kill_all, "engage survives restart");
        assert!(second.status().revision > r3);
        assert!(second.check_tool("shell").is_err());
    }

    #[test]
    fn corrupted_state_defaults_to_fail_closed_kill_all() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        fs::write(&state_path, "{not-valid-json").unwrap();
        let cfg = estop_config(&state_path);
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();
        assert!(authority.status().state.kill_all);
        assert!(authority.check_tool("shell").is_err());
    }

    #[test]
    fn a_hand_edited_state_file_with_mixed_case_names_still_freezes_the_tool() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        fs::write(
            &state_path,
            r#"{"frozen_tools": [" Shell ", "FILE_WRITE"]}"#,
        )
        .unwrap();
        let authority = EstopAuthority::new(&estop_config(&state_path), dir.path(), None).unwrap();

        assert!(authority.check_tool("shell").is_err());
        assert!(authority.check_tool("file_write").is_err());
        assert!(authority.check_tool("memory_recall").is_ok());
        assert_eq!(
            authority.status().state.frozen_tools,
            vec!["file_write".to_string(), "shell".to_string()]
        );
    }

    #[test]
    fn unreadable_state_path_fails_closed_not_open() {
        let dir = tempdir().unwrap();
        // A directory where the state file should be: reading it is an I/O
        // error that is not `NotFound`.
        let state_path = dir.path().join("estop-state.json");
        fs::create_dir(&state_path).unwrap();
        let cfg = estop_config(&state_path);
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();
        assert!(authority.status().state.kill_all);
    }

    #[test]
    fn missing_state_file_means_never_engaged() {
        let dir = tempdir().unwrap();
        let cfg = estop_config(&dir.path().join("absent.json"));
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();
        assert!(!authority.status().state.is_engaged());
    }

    #[test]
    fn legacy_unenforced_fields_are_kept_reported_and_clearable() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        fs::write(
            &state_path,
            r#"{"network_kill":true,"blocked_domains":["*.paypal.com"]}"#,
        )
        .unwrap();
        let cfg = estop_config(&state_path);
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();

        let status = authority.status();
        assert!(status.state.has_unenforced());
        assert!(!status.state.kill_all && status.state.frozen_tools.is_empty());
        assert!(authority.check_tool("shell").is_ok());

        authority.resume(ResumeSelector::Network, None).unwrap();
        authority
            .resume(ResumeSelector::Domains(vec!["*.paypal.com".into()]), None)
            .unwrap();
        assert!(!authority.status().state.has_unenforced());
    }

    #[test]
    fn engage_reports_non_durable_state_instead_of_failing_open() {
        let dir = tempdir().unwrap();
        // The state file path is a directory: `rename` onto it fails.
        let state_path = dir.path().join("blocked");
        let cfg = estop_config(&state_path);
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();
        fs::create_dir(&state_path).unwrap();

        let snapshot = authority.engage(EstopLevel::KillAll).unwrap();
        assert!(!snapshot.durable);
        assert!(snapshot.persist_error.is_some());
        assert!(
            authority.check_tool("shell").is_err(),
            "engage is live in memory even when it could not be persisted"
        );
    }

    #[test]
    fn failed_resume_persist_leaves_the_stop_in_place() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let cfg = estop_config(&state_path);
        let authority = EstopAuthority::new(&cfg, dir.path(), None).unwrap();
        authority.engage(EstopLevel::KillAll).unwrap();

        fs::remove_file(&state_path).unwrap();
        fs::create_dir(&state_path).unwrap();
        assert!(authority.resume(ResumeSelector::KillAll, None).is_err());
        assert!(authority.check_tool("shell").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_private_and_leaves_no_temporary_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let authority = EstopAuthority::new(&estop_config(&state_path), dir.path(), None).unwrap();

        authority.engage(EstopLevel::KillAll).unwrap();
        authority.resume(ResumeSelector::KillAll, None).unwrap();

        let mode = fs::metadata(&state_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the state file must not be readable by others");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "estop-state.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temporary files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn concurrent_engage_and_resume_keep_memory_and_file_in_step() {
        const THREADS: usize = 8;
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let authority = EstopAuthority::new(&estop_config(&state_path), dir.path(), None).unwrap();
        let start = authority.status().revision;

        let revisions: Vec<u64> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|i| {
                    let authority = &authority;
                    scope.spawn(move || {
                        let tool = format!("tool_{i}");
                        let engaged = authority
                            .engage(EstopLevel::ToolFreeze(vec![tool.clone()]))
                            .unwrap();
                        assert!(engaged.durable, "{:?}", engaged.persist_error);
                        let resumed = authority
                            .resume(ResumeSelector::Tools(vec![tool]), None)
                            .unwrap();
                        vec![engaged.revision, resumed.revision]
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect()
        });

        // Every transaction got its own revision, with no gaps or repeats.
        let mut sorted = revisions.clone();
        sorted.sort_unstable();
        let expected: Vec<u64> = (start + 1..=start + 2 * THREADS as u64).collect();
        assert_eq!(sorted, expected, "revisions must be unique and contiguous");

        // Memory and the state file agree on the final state.
        let status = authority.status();
        assert_eq!(status.revision, start + 2 * THREADS as u64);
        assert!(status.state.frozen_tools.is_empty());
        let on_disk: EstopState =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(on_disk.revision, status.revision);
        assert_eq!(on_disk.frozen_tools, status.state.frozen_tools);
    }

    #[test]
    fn scope_lists_belong_to_their_own_level() {
        let tools = vec!["shell".to_string()];
        let domains = vec!["x.com".to_string()];
        let none: Vec<String> = Vec::new();

        for level in ["kill-all", "network-kill", "domain-block"] {
            assert_eq!(
                misplaced_scope(level, &tools, &none),
                Some(MisplacedScope::Tools),
                "{level} must not take tools"
            );
        }
        for level in ["kill-all", "network-kill", "tool-freeze"] {
            assert_eq!(
                misplaced_scope(level, &none, &domains),
                Some(MisplacedScope::Domains),
                "{level} must not take domains"
            );
        }
        assert_eq!(misplaced_scope("tool-freeze", &tools, &none), None);
        assert_eq!(misplaced_scope("domain-block", &none, &domains), None);
        assert_eq!(misplaced_scope("kill-all", &none, &none), None);
        // An unknown level is rejected by the caller, not mistaken for a scope error.
        assert_eq!(misplaced_scope("bogus", &tools, &domains), None);
    }

    #[test]
    fn new_requires_a_validator_when_otp_is_required() {
        let dir = tempdir().unwrap();
        let mut cfg = estop_config(&dir.path().join("estop-state.json"));
        cfg.require_otp_to_resume = true;
        let error = EstopAuthority::new(&cfg, dir.path(), None).unwrap_err();
        assert!(error.to_string().contains("no OTP secret is enrolled"));
        assert!(
            !crate::security::otp::secret_file_path(dir.path()).exists(),
            "the authority must not mint an OTP secret"
        );
    }

    #[test]
    fn resume_requires_valid_otp_when_enabled() {
        let dir = tempdir().unwrap();
        let mut cfg = estop_config(&dir.path().join("estop-state.json"));
        cfg.require_otp_to_resume = true;
        let authority =
            EstopAuthority::new(&cfg, dir.path(), Some(otp_validator(dir.path()))).unwrap();
        authority.engage(EstopLevel::KillAll).unwrap();

        let error = authority
            .resume(ResumeSelector::KillAll, None)
            .expect_err("resume should require OTP");
        assert!(error.to_string().contains("OTP code is required"));
        assert!(authority.status().state.kill_all);
    }

    #[test]
    fn a_resume_with_no_code_asks_for_one_and_never_counts_as_a_failed_attempt() {
        let dir = tempdir().unwrap();
        let mut cfg = estop_config(&dir.path().join("estop-state.json"));
        cfg.require_otp_to_resume = true;
        let validator = otp_validator(dir.path());
        let code = validator.code_for_timestamp(now_secs());
        let authority = EstopAuthority::new(&cfg, dir.path(), Some(validator)).unwrap();
        authority.engage(EstopLevel::KillAll).unwrap();

        // More refusals than the lockout allows wrong codes.
        for missing in [None, Some(""), Some("   "), None, None, None] {
            let error = authority
                .resume(ResumeSelector::KillAll, missing)
                .expect_err("a resume with no code is refused");
            assert!(error.is::<OtpRequired>(), "{error:#}");
            assert!(!error.is::<ResumeDenied>(), "{error:#}");
            assert!(authority.status().state.kill_all);
        }

        // None of them locked the verifier: the right code still works.
        authority
            .resume(ResumeSelector::KillAll, Some(&code))
            .expect("the code is accepted after any number of missing-code attempts");
        assert!(!authority.status().state.kill_all);
    }

    #[test]
    fn a_wrong_code_is_denied_not_asked_for_again() {
        let dir = tempdir().unwrap();
        let mut cfg = estop_config(&dir.path().join("estop-state.json"));
        cfg.require_otp_to_resume = true;
        let authority =
            EstopAuthority::new(&cfg, dir.path(), Some(otp_validator(dir.path()))).unwrap();
        authority.engage(EstopLevel::KillAll).unwrap();

        let error = authority
            .resume(ResumeSelector::KillAll, Some("000000"))
            .expect_err("a wrong code is refused");
        assert!(error.is::<ResumeDenied>(), "{error:#}");
        assert!(!error.is::<OtpRequired>(), "{error:#}");
    }

    #[test]
    fn resume_accepts_a_valid_otp_code_once() {
        let dir = tempdir().unwrap();
        let mut cfg = estop_config(&dir.path().join("estop-state.json"));
        cfg.require_otp_to_resume = true;
        let validator = otp_validator(dir.path());
        let code = validator.code_for_timestamp(now_secs());
        let authority = EstopAuthority::new(&cfg, dir.path(), Some(validator)).unwrap();

        authority.engage(EstopLevel::KillAll).unwrap();
        authority
            .resume(ResumeSelector::KillAll, Some(&code))
            .unwrap();
        assert!(!authority.status().state.kill_all);

        authority.engage(EstopLevel::KillAll).unwrap();
        let replay = authority
            .resume(ResumeSelector::KillAll, Some(&code))
            .expect_err("the same code must not authorize a second resume");
        assert!(replay.to_string().contains("Invalid OTP code"));
        assert!(authority.status().state.kill_all);
    }
}
