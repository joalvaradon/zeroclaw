//! A cron job scheduled from inside a bounded delegate must not outlive the
//! caller's tool ceiling.
//!
//! Bounded delegation caps the target's registry by the caller's own. That cap
//! lives in the turn. A cron job does not: when it fires, the scheduler rebuilds
//! the owning agent's policy from config and hands `agent::run` the job's STORED
//! `allowed_tools`, so a job saved without one runs with the owning agent's full
//! registry. A bounded target can therefore schedule work that executes tools
//! its caller was never granted — the deferred half of the same escape the
//! in-turn ceiling closes.
//!
//! The scenario: `caller` may `delegate` and `cron_add` but has no
//! `file_write`. `target` permits both `cron_add` and `file_write`. The caller
//! delegates bounded work; inside that bounded sub-loop the model asks
//! `cron_add` for a job whose `allowed_tools` names `file_write`.
//!
//! THIS TEST MUST FAIL if the stored tool set stops being capped. Neutralize it
//! by making `CronAddTool::cap_allowed_tools` return its argument unchanged, and
//! the stored job regains `file_write`.
//!
//! Both halves are asserted, because under a cap that stored nothing at all the
//! negative half would hold for the wrong reason:
//!   - negative: the stored list must NOT contain `file_write`;
//!   - positive: it MUST contain `cron_add`, which proves the job was really
//!     created through the bounded target's rebuilt tool rather than the whole
//!     path failing somewhere earlier.
//!
//! What this does NOT assert: it does not run the scheduler, so it pins the
//! stored bound rather than the behaviour of a fired job. It also says nothing
//! about `cron_run`, whose refusal is a different property covered by the unit
//! tests of `tools::caller_ceiling`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, DelegateExecutionMode, DelegateTargetConfig, RiskProfileConfig,
    RuntimeProfileConfig,
};
use zeroclaw_runtime::agent::loop_::AgentRunOverrides;

/// The tool the caller was never granted and the target would otherwise
/// smuggle into a scheduled job.
const BEYOND_CEILING: &str = "file_write";

/// A tool both sides hold, so the job is created with a non-empty stored list
/// and the positive half of the assertion has something to observe.
const WITHIN_CEILING: &str = "cron_add";

const JOB_NAME: &str = "bounded-scheduled-work";

#[derive(Clone)]
struct Script {
    calls: Arc<AtomicUsize>,
    captured: Arc<AsyncMutex<Vec<String>>>,
    /// Canned replies, consumed in order. Once exhausted every further turn
    /// answers plainly so each loop in the chain unwinds instead of hanging.
    replies: Arc<Vec<String>>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-cron",
        "object": "chat.completion",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": serde_json::Value::Null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string()
}

fn plain_content(text: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-cron",
        "object": "chat.completion",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string()
}

async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    script
        .replies
        .get(call)
        .cloned()
        .unwrap_or_else(|| plain_content("done"))
}

/// The `cron_add` call the deepest loop of each chain makes. It asks for a tool
/// its caller never held, which is the whole point of the assertion.
fn schedule_out_of_ceiling_job() -> String {
    native_tool_call(
        "cron_add",
        &serde_json::json!({
            "name": JOB_NAME,
            "schedule": {"kind": "cron", "expr": "*/5 * * * *"},
            "prompt": "carry out the scheduled work",
            "allowed_tools": [BEYOND_CEILING, WITHIN_CEILING],
        })
        .to_string(),
    )
}

async fn spawn_stub_provider(replies: Vec<String>) -> SocketAddr {
    let script = Script {
        calls: Arc::new(AtomicUsize::new(0)),
        captured: Arc::new(AsyncMutex::new(Vec::new())),
        replies: Arc::new(replies),
    };
    let app = Router::new()
        .route("/chat/completions", post(handle_chat))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    zeroclaw_spawn::spawn!(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    addr
}

fn bounded_cron_config(provider_uri: &str, root: &std::path::Path) -> Config {
    let mut providers = zeroclaw_config::providers::Providers::default();
    {
        let base = providers
            .models
            .ensure("custom", "default")
            .expect("`custom` slot must exist on ModelProviders");
        base.api_key = Some("test-key".to_string());
        base.model = Some("test-model".to_string());
        base.uri = Some(provider_uri.to_string());
        base.native_tools = Some(true);
    }

    let permissive = |tools: Vec<String>| RiskProfileConfig {
        allowed_tools: tools,
        delegation_policy: DelegationPolicy {
            mode: DelegationMode::Allow,
        },
        ..RiskProfileConfig::default()
    };

    let mut risk_profiles = HashMap::new();
    // The caller can delegate and schedule, but cannot write files. This is the
    // ceiling the scheduled job must not exceed.
    risk_profiles.insert(
        "caller_profile".to_string(),
        permissive(vec![
            "delegate".to_string(),
            "spawn_subagent".to_string(),
            WITHIN_CEILING.to_string(),
        ]),
    );
    // The target's own profile is wider. Without the cap, the job it schedules
    // inherits THIS, which is the defect.
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec![
            "spawn_subagent".to_string(),
            WITHIN_CEILING.to_string(),
            BEYOND_CEILING.to_string(),
        ]),
    );

    let mut runtime_profiles = HashMap::new();
    runtime_profiles.insert(
        "agentic".to_string(),
        RuntimeProfileConfig {
            agentic: true,
            max_tool_iterations: 3,
            ..RuntimeProfileConfig::default()
        },
    );

    let mut agents = HashMap::new();
    agents.insert(
        "caller".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "caller_profile".into(),
            runtime_profile: "agentic".into(),
            delegates: vec![DelegateTargetConfig {
                agent: "target".to_string(),
                mode: DelegateExecutionMode::Bounded,
            }],
            ..AliasedAgentConfig::default()
        },
    );
    agents.insert(
        "target".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "target_profile".into(),
            runtime_profile: "agentic".into(),
            ..AliasedAgentConfig::default()
        },
    );

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    let mut config = Config {
        data_dir,
        config_path: root.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        runtime_profiles,
        ..Config::default()
    };
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    config
}

/// Returns the stored `allowed_tools` of the job the bounded target scheduled,
/// plus a report string for assertion failures.
async fn drive_bounded_cron_add(replies: Vec<String>) -> (Option<Vec<String>>, String) {
    let tmp = TempDir::new().expect("temp root");
    let addr = spawn_stub_provider(replies).await;
    let config = bounded_cron_config(&format!("http://{addr}"), tmp.path());
    // `agent::run` consumes the config; the store is read back through this copy.
    let reader = config.clone();

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the scheduling to the target agent".to_string()),
        None,
        None,
        None,
        vec![],
        false,
        None,
        None,
        zeroclaw_api::ingress::TurnOrigin::SubTurn,
        AgentRunOverrides::default(),
    )
    .await;

    let jobs = zeroclaw_runtime::cron::list_jobs(&reader).unwrap_or_default();
    let report = format!(
        "outcome {outcome:?}; jobs {:?}",
        jobs.iter()
            .map(|j| (j.name.clone(), j.agent_alias.clone(), j.allowed_tools.clone()))
            .collect::<Vec<_>>()
    );
    let stored = jobs
        .into_iter()
        .find(|job| job.name.as_deref() == Some(JOB_NAME))
        .and_then(|job| job.allowed_tools);
    (stored, report)
}

/// Nested delegation stacks futures past the harness default per-thread stack.
fn drive_blocking(replies: Vec<String>) -> (Option<Vec<String>>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_bounded_cron_add(replies))
            .await
            .expect("chain task joins")
    })
}

/// Shared by both chains: the stored list must have been capped, and must not
/// be empty or absent — either of which the scheduler reads as unrestricted,
/// so the negative half alone would hold for exactly the wrong reason.
fn assert_stored_within_ceiling(stored: Option<Vec<String>>, report: &str, chain: &str) {
    let stored = stored.unwrap_or_else(|| {
        panic!(
            "[{chain}] no job with an allowed_tools list was stored; the negative \
             assertion would hold vacuously. {report}"
        )
    });
    assert!(
        stored.iter().any(|t| t == WITHIN_CEILING),
        "[{chain}] the admitted tool did not survive the cap, so the job was not \
         really scheduled through the bounded path; stored={stored:?}; {report}"
    );
    assert!(
        !stored.iter().any(|t| t == BEYOND_CEILING),
        "[{chain}] a job scheduled from inside a bounded delegate stored \
         `{BEYOND_CEILING}`, which the caller was never granted: the ceiling did not \
         survive to the persisted job; stored={stored:?}; {report}"
    );
}

#[test]
fn a_job_scheduled_from_a_bounded_delegate_stores_only_tools_within_the_caller_ceiling() {
    let (stored, report) = drive_blocking(vec![
        native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"schedule the follow-up"}"#,
        ),
        schedule_out_of_ceiling_job(),
    ]);
    assert_stored_within_ceiling(stored, &report, "caller -> bounded target -> cron_add");
}

/// The composition the two review-requested regressions do not reach.
///
/// `spawn_subagent` now carries the sealed set, so the nested child's own
/// registry is correctly bounded — that is this PR's other repair. But the
/// child can still SCHEDULE, and a stored job is read back by the scheduler
/// long after every one of those in-turn bounds is gone. Capping the child's
/// registry therefore does not cap what the child persists: without the stored
/// cap, this chain defeats the very fix that bounds the hop before it.
///
/// THIS TEST MUST FAIL if `cron_add` stops capping the stored list, even while
/// the direct `caller -> target -> cron_add` chain above is still repaired.
#[test]
fn a_job_scheduled_from_a_spawned_child_of_a_bounded_delegate_inherits_the_same_ceiling() {
    let (stored, report) = drive_blocking(vec![
        native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"hand this to a subagent"}"#,
        ),
        native_tool_call("spawn_subagent", r#"{"prompt":"schedule the follow-up"}"#),
        schedule_out_of_ceiling_job(),
    ]);
    assert_stored_within_ceiling(
        stored,
        &report,
        "caller -> bounded target -> spawn_subagent -> cron_add",
    );
}
