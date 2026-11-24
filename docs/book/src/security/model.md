# The security model

ZeroClaw's security model gates what the agent is allowed to do at runtime. There are six layers. From outer to inner:

## Channel pairing and access control

Before a message from a channel reaches the agent, the channel's pairing and allow-list are checked. `allowed_users`, `allowed_chats`, IP allowlists for webhooks, all enforced at the channel adapter, before the runtime sees the event.

Docs: each channel's page under [Channels](../channels/overview.md).

## Autonomy level

The coarse-grained knob. Three settings:

- **ReadOnly**: the agent can observe (read files, query memory, fetch URLs it's allowed to fetch) but cannot write or execute commands.
- **Supervised** (default): low-risk ops run; medium-risk ask the operator; high-risk block.
- **Full**: uncovered tools skip approval gates; `always_ask` still prompts or fails closed. `workspace_only` is implicitly disabled. `forbidden_paths`, `forbidden_commands`, and the OS sandbox still enforce.

Docs: [Autonomy levels](./autonomy.md).

## Workspace boundary and path rules

The agent operates within a configured workspace directory. `file_read`, `file_write`, and `shell` (for commands that touch the filesystem) refuse paths outside it unless `workspace_only = false`.

**Per-session sandbox roots (ACP and gateway WebSocket):** When a session is opened via ACP (`session/new` with a `cwd` parameter) or via the gateway WebSocket (connect-time `cwd` parameter), that path becomes the `SecurityPolicy` workspace boundary for all file and shell tools for the lifetime of the session. The daemon's global `workspace_dir` remains the data directory for memory, identity, cron, and other persistent state. The model is: `session cwd` = project boundary the agent can touch; `workspace_dir` = where ZeroClaw stores its own files. Note: the agent's system prompt currently reflects the daemon's `workspace_dir` rather than the session `cwd`; enforcement is correct but the model's self-reported location may differ.

**Important:** the `cwd` parameter changes which directory on the **ZeroClaw host** the agent is sandboxed to, it does not affect which machine tools run on. Tool use (shell commands, file reads/writes) always executes on the machine running ZeroClaw. If you connect to a remote ZeroClaw instance over the gateway WebSocket, tool calls operate on the remote machine's filesystem, not on your local machine. For localhost-only deployments this distinction does not matter, but remote setups should account for it.

Beyond the workspace, `forbidden_paths` defaults include `/etc`, `/sys`,
`/boot`, `~/.ssh`, and other sensitive roots. Absolute allow and forbidden
entries use component-prefix specificity: the most specific matching entry
wins, with forbidden winning an equal-depth tie. This lets a nested forbidden
subtree block part of the workspace or an allowed root without making broad
defaults such as `/home` override a narrower operator-configured allow.

## Shell command policy

For shell invocations:

- `allowed_commands`: if non-empty, shell only runs commands whose basename is in this list
- `forbidden_commands`: explicit denylist (`rm -rf /`, `shutdown`, kernel operations)
- `validate_command_execution`: a pattern-matching pass that looks for dangerous flags, pipelines, and argument shapes

The validator runs *before* the command hits the shell. A blocked command surfaces as a tool error the model sees and can react to.

## OS-level sandbox

When a sandbox backend is available, tool invocations run inside it. `sandbox_backend = "auto"` resolves one when the sandbox is created:

| Platform | Auto-detected order |
|---|---|
| Linux | Landlock (only when compiled with `sandbox-landlock`) → Firejail |
| macOS | Bubblewrap (only when compiled with `sandbox-bubblewrap`) → Seatbelt (`sandbox-exec`) |
| Windows | No OS-level backend |

Docker comes after the platform list and only when the runtime kind is neither `native` nor `docker`, so the native runtime never falls back to it. When no backend is available, the native runtime uses application-layer security only (`none`), while the Docker runtime keeps its container boundary (`docker-runtime`).

The sandbox confines filesystem access to the workspace and removes access to the parent process's secrets. Network handling varies by backend; see [Sandboxing](./sandboxing.md#network).

Docs: [Sandboxing](./sandboxing.md).

## Tool receipts

Tool receipts provide HMAC evidence that a successful tool call and its result passed through the runtime. When receipts are enabled, successful tool outputs receive an HMAC-SHA256 receipt over the call and result, and the receipt is fed back into the conversation with the tool result.

Receipts help catch fabricated tool claims. They are not a chained or durable audit log today: receipt keys are ephemeral, receipts are not cross-signed with the conversation hash, and persistent receipt storage is still future work.

Docs: [Tool receipts](./tool-receipts.md).

## Additional gates

Beyond the six layers:

- **OTP gating**: `[security.otp] gated_actions = ["shell", "browser", "file_write"]` requires a one-time code before each listed action. Useful for remote-access scenarios.
- **Emergency stop**: with `security.estop.enabled = true` the daemon owns the emergency-stop state, and `zeroclaw estop` (engage, `status`, `resume`) talks to that running daemon over the local RPC socket. With no daemon reachable the command fails and changes nothing.
  - **What it refuses.** While a stop is engaged the daemon refuses new tool calls. `kill-all` refuses every tool call, scheduled shell job and SOP capability step; `--level tool-freeze --tool <name>` refuses the named tools, including when they are reached through a pipeline or a skill alias. Freezing `shell` also refuses scheduled shell jobs, but not SOP capability steps. A refused scheduled shell job is deferred, not run and not recorded: a one-shot job stays enabled and a recurring job keeps its schedule, so both run once the stop is lifted.
  - **External coding agents.** `kill-all` also refuses to start the model providers that run an external coding agent (the ACP providers `grok_cli` and `antigravity_cli`, and `kilocli`), and answers an allow-once permission request from a running ACP agent with a refusal. A tool freeze does not reach them, because their tools are not ZeroClaw tools. A credential helper that a provider runs to authenticate, such as Bedrock's `credential_process`, is not gated.
  - **What it does not do.** It does **not** interrupt a call that is already running, and `network-kill` and `domain-block` are not enforced (the command refuses to engage them). To halt a running call now, stop the ZeroClaw process itself.
  - **Commands that refuse to start.** Commands that run tools in their own process instead of the daemon (`zeroclaw agent`, `zeroclaw acp`, `zeroclaw gateway` / `gateway start`, `zeroclaw channel start`) refuse to start while `security.estop.enabled` is set, because they could not honor a stop.
  - **Resuming.** `resume` requires a one-time OTP when `require_otp_to_resume` is set (the default), which needs an enrolled OTP secret. To enroll one, set `security.otp.enabled = true` and start any zeroclaw command once: it prints an enrollment URI on standard output (for a service, that is in its log). A secret file that already exists is used even when `security.otp.enabled` is false. `zeroclaw daemon` refuses to start with estop enabled, `require_otp_to_resume` set and no secret. After `security.otp.challenge_max_attempts` consecutive wrong codes `resume` is locked for 5 minutes. Without `--otp`, `resume` first asks the daemon, and on a terminal prompts for the code only when the daemon says it needs one (the code is not echoed on Unix); a request that carried no code is refused without counting as a wrong code.
  - **What is kept in memory.** The codes already used and the lockout counter live in the daemon's memory, so a restart clears them: a code that was used can be accepted again after a restart while it is still valid. Whether estop exists, and `state_file` and `require_otp_to_resume`, are decided once when the process starts; a config reload does not change them and a change takes a restart, and rotating the OTP secret takes a restart too. If the state file cannot be written the stop stays active in memory and is reported as not durable; it is written again only by the next engage or resume. Keep `state_file` outside the agent's workspace: only the default file name `estop-state.json` in the config directory is protected against agent writes.
  - **Who can resume.** The local RPC socket treats any process of the same OS user as an operator when no roster is configured, so such a process (including a tool the agent runs as that user) can run `zeroclaw estop resume` when `require_otp_to_resume` is false, and can lock `resume` for 5 minutes by sending wrong codes. With OTP, prefer `--otp -` (stdin) over a code on the command line, which other processes of that user can read. Every engage and resume is logged with the sender's principal id, and so is every refused resume attempt (never the code).
- **Prompt injection guard**: scans model output for known injection patterns before tool calls are validated.
- **Leak detector**: scans outbound channel responses for credentials and redacts matches before delivery. It covers deterministic credential patterns and can also run a standalone high-entropy-token heuristic.
- **Pairing guard**: device pairing for channel auth; prevents stolen credentials from working on a new device.

## Leak detector configuration

Configure outbound leak detection in its own TOML section:

```toml
[security.leak_detection]
enabled = true
sensitivity = 0.7
high_entropy_tokens = true
```

`enabled = false` disables the entire outbound leak detector.
`high_entropy_tokens = false` disables only the standalone entropy heuristic;
deterministic credential patterns still run. `sensitivity` accepts `0.0`
through `1.0`; higher values are more aggressive.

The complete field table and defaults are in the
[Config reference](../reference/config.md#securityleak_detection).

## When things go wrong

A blocked tool call doesn't silently fail:

1. The security validator returns an error
2. The runtime wraps it as a `ToolResult::Err` and hands it back to the model
3. The model sees "Error: Shell command blocked by policy: forbidden pattern `rm -rf /`" and can retry, apologise, or ask the user

If a tool is excluded from the channel via `[autonomy].non_cli_excluded_tools` (which gates non-CLI channels as a group), it simply isn't advertised to the model on those channels. Model never sees a tool it can't use.

## Default posture

Out of the box:

- Autonomy: `Supervised`
- Workspace-only: `true`
- Sandbox: auto-detect (uses whatever the OS provides)
- Audit logging: `true`, covering certificate issuance and renewal only (command execution is not audited)
- OTP: `false`
- E-stop: `false`

This is a reasonable middle ground, safe enough for a laptop, permissive enough to not frustrate. For production, enable OTP and restricted tools, and leave `[security.audit]` on so certificate issuance and renewal stay recorded. Command execution is not audited: until it has a production writer, use an external supervisor or logging wrapper that observes the ZeroClaw process, or enable OS-level process accounting. For a development box, you can instead opt down to [YOLO](../getting-started/yolo.md).
