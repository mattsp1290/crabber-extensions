# Crabber Extensions

Trusted native extensions for [Crabber](https://github.com/mattsp1290/crabber).
The first implementation slice provides workspace instructions, bounded
ask-user interaction, bounded delegated tasks, bounded web search, and final
JSON tool-result redaction, plus bounded command syntax analysis and runtime
enforcement and bounded background jobs. Two additional features remain planned in
[the nine-feature parity plan](docs/extension-parity.md).

The workspace consumes only published Crabber public APIs, pinned to
`883189465397b2fd326a6ef358c8a2fbd39fbec8`. The pinned source is public and
CI fetches it over HTTPS without a read token. Tests and the example use scripted providers
and synthetic data; no provider or API credentials are required.

```sh
cargo test --workspace --locked
cargo run -p crabber-extensions --example protected_workspace --locked
```

## Workspace instructions

`workspace_instructions::WorkspaceInstructions` implements Crabber's `Extension`
trait. Mount it with `AgentBuilder::extension` or `Registry::mount`. Supply
explicit `Options`, including a host `Resolver`, a non-secret resolver identity,
ordered file basenames and finite limits. The example shows a complete mount.

The host opens `TrustedWorkspace::open(boundary, root)` and persists its canonical
`root()` as the Crabber session directory. A resolver receives the authoritative
`PromptAttemptContext` each attempt and returns an admitted capability, or `None`
to withhold instructions. The returned root must match the persisted directory
exactly. Workspace ID and directory are routing data; the resolver decides trust.
Missing identity must never be replaced with host defaults or model arguments.

Directory handles pin the admitted boundary-to-root chain. Regular files are
read afresh on every main-model attempt, in directory then configured basename
order. Final symlinks, missing/unreadable files, invalid UTF-8, NUL-bearing files
and empty content are skipped. Files are truncated on a UTF-8 boundary with an
explicit marker; sections omit whole files with an explicit marker when full.
Relative display paths avoid echoing absolute roots. Contents are trusted prompt
text and are not escaped or interpreted as a security boundary.

`workspace/instructions` is the registration name. A session mount shadows a
global mount with that name. Crabber rebuilds each main-model attempt from its
stable base and current contributions, including retries and post-compaction
attempts; internal compaction summary requests omit dynamic contributors.

Bounds are required: at most 16 configured names, 64 directories, 16 KiB per file,
32 KiB per rendered section, 256 in-flight operations and 5 seconds caller wait.
The section cap must be at least 256 bytes. Smaller configured values apply.
Capacity saturation and resolver/deadline failures stop the model attempt.
Blocking reads run off the async executor. A blocked filesystem read or a host
resolver that ignores cancellation retains its task and capacity until it ends;
mount close tracks that work and may time out. Host async resolvers must yield.
Capabilities constrain reads using [cap-std directory handles](https://docs.rs/cap-std/4.0.3/cap_std/fs/struct.Dir.html).
Hard links, trusted contents and host admission races remain host concerns.

## Tool result redactor

`tool_result_redactor::ToolResultRedactor` also implements `Extension`. It
registers in Crabber's **final-redaction phase**, after ordinary reducers
regardless of their registration order. Runtime identity and sticky error status
remain authoritative; the redactor changes only JSON result values.

The built-in catalog matches PKCS#8 private/encrypted-private-key PEM blocks,
Authorization Bearer headers, GitHub prefixed tokens and stateless GitHub tokens,
with token-boundary checks. Hosts can add bounded non-empty regex patterns.
Patterns that can match zero bytes are rejected. These rules are explicit
pattern protection, not a guarantee that arbitrary secrets will be recognized.

Strings are scanned recursively. A secret-bearing or unsafe object key replaces
its containing object with `[REDACTED]`, avoiding key renaming/collisions. Oversized
or NUL-bearing strings become the placeholder. Depth, node or total-byte
exhaustion replaces the entire result, preventing partially scanned output.
Finite bounds apply before and after replacement; byte caps must leave room for
the fixed placeholder. Nodes include keys; the total
byte budget counts decoded strings/keys and JSON primitives, not wire escaping.

Exact tool exclusions deliberately bypass protection. Configuration hashes cover
patterns, exclusions, bounds and order. Capacity is shared across mounts of the
same extension instance. Scanning during runtime dispatch runs in
tracked blocking work, with capacity held until that work ends. Saturation or
worker failure returns a placeholder; runtime cancellation/failure settlement
remains Crabber's responsibility. The standalone synchronous `redact` helper
uses the same scan policy without runtime lifecycle/concurrency admission.

The redactor protects result payloads before persistence and next-model input.
It does not erase inputs, tool names, model output, logs, binary artifacts or
filesystem contents. Crabber supplies parsed JSON values; no Eino attachment or
raw-JSON envelope is introduced. Removing protection requires host policy and
settling active runs; use `Agent::close_extensions` for terminal registry close.

## Command guard

`command_guard::Policy` is a pure, deny-only analysis library for hosts:

```rust
use crabber_extensions::command_guard::{default_bindings, Limits, Options, Policy, Rule};

let policy = Policy::new(Options {
    bindings: default_bindings(),
    rules: vec![Rule {
        id: "host-git-push".into(), executable: "git".into(),
        arg_prefix: vec!["push".into()],
    }],
    limits: Limits {
        max_bindings: 8, max_rules: 32, max_rule_bytes: 2048, max_prefix_args: 16,
        max_json_depth: 16, max_json_nodes: 256, max_command_bytes: 4096,
        max_analysis_bytes: 8192, max_ast_nodes: 2048, max_ast_depth: 16,
        max_words: 512, max_word_bytes: 4096, max_wrapper_depth: 8, max_in_flight: 4,
    },
})?;
let outcome = policy.analyze("shell", &serde_json::json!({"cmd": "git push origin"}));
assert!(outcome.denies());
```

An abstention is never an approval. Unsupported syntax and exhausted budgets
deny without returning command text. The hand-written parser has no new
dependencies and caps nesting at 32, supported by 512 KiB debug-stack tests.
`config_hash()` covers every setting and versioned analysis behavior. No process,
filesystem, environment or network access occurs during analysis.

Mount the deny-only wrapper with host-selected scope:

```rust,ignore
use std::sync::Arc;
use crabber::{Agent, extension::Scope};
use crabber_extensions::command_guard::CommandGuard;

let guard = Arc::new(CommandGuard::new(options)?);
let builder = Agent::builder().extension(guard.clone(), Scope::Global);
let stats = guard.stats();
```

Denials persist as Crabber's fixed `permission denied`, without a reason.
Hosts re-run `guard.policy().analyze(tool_name, &stored_arguments)` for the
analysis class; capacity and internal failures appear only in `stats()`.
Scope and permissions remain host-owned. Use one instance per tenant to
isolate capacity and counters; shared mounts share both. Each synchronous
check blocks its worker. Use multi-thread runtimes and size `max_in_flight`
at or below the workers the host can afford to block. Panics are caught only
with unwinding enabled; the host-owned panic hook runs before containment.
Policy, wrapper behavior identity or crate-version changes invalidate paused
run fingerprints. Settle unfinished runs before upgrading or rolling back.

This is trusted syntax inspection, not a sandbox. Known non-matches include
`git -C . push`, `nohup`, `nice`, `ionice`, `xargs`, `find -exec`, `busybox`, `ssh`,
`python -c`, `perl -e`, `node -e`, `make`, host-defined shell functions/aliases,
and different executable basenames. See [parity differences](docs/extension-parity.md#command-syntax-analysis)
and the [reference fixture procedure](crates/extensions/tests/command_guard/fixtures/README.md).
Unix shell tests require Bash 5+; set `COMMAND_GUARD_REQUIRE_SHELLS=1` to require
execution instead of an explicit skip. CI configuration remains a host decision.

## Background jobs

`background_jobs::BackgroundJobs` owns bounded, non-interactive POSIX shell
jobs on Linux and macOS. Hosts provision the shell and explicitly choose the
frozen environment, policy identities and resource bounds:

```rust,ignore
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use crabber::{Agent, extension::Scope};
use crabber_extensions::background_jobs::{
    BackgroundJobs, Environment, EnvironmentMode, Limits, Options,
};

let jobs = Arc::new(BackgroundJobs::new(Options {
    shell_path: "/bin/sh".into(),
    shell_identity: "host-posix-shell-v1".into(),
    environment: Environment {
        mode: EnvironmentMode::ExplicitOnly,
        overrides: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        identity: "host-job-environment-v1".into(),
    },
    limits: Limits {
        max_running: 2, max_tracked: 16,
        max_command_bytes: 4096, max_working_directory_bytes: 1024,
        max_output_bytes_per_stream: 65536,
        max_environment_entries: 64, max_environment_bytes: 16384,
        default_timeout: Duration::from_secs(30),
        max_timeout: Duration::from_secs(300),
        terminate_grace: Duration::from_millis(500),
        kill_wait: Duration::from_secs(3),
        shutdown_grace: Duration::from_secs(5),
    },
})?);
let builder = Agent::builder().extension(jobs.clone(), Scope::Global);
// Supply the host's store, provider, permission policy and AgentConfig.
```

| Tool | Behavior | Permission metadata |
| --- | --- | --- |
| `background_job_start` | Start `command`, optional relative `working_directory` and whole-second `timeout_seconds`; return an opaque job ID immediately. | `background.process.start` |
| `background_job_status` | Read one owned ID, terminal exit status and bounded stdout/stderr tails. | `background.process.read` |
| `background_job_list` | List tracked jobs for the current owner in start order. | `background.process.read` |
| `background_job_kill` | TERM the whole group, then KILL/reap; repeated terminal kills report `newly_accepted: false`. | `background.process.kill` |

Owners are the authoritative durable session ID and workspace ID. The initial
working directory resolves beneath the persisted workspace directory, including
symlink checks. This is launch validation only, not a sandbox: commands can
access anything allowed by the host OS. Hosts own filesystem/network trust,
credentials, provisioning and permission decisions. `Ask` denies under Agent.
Mount CommandGuard to inspect starts and the result redactor to protect returned
tails before persistence. Raw in-memory tails and command arguments remain
unredacted.

| State | Meaning |
| --- | --- |
| `running` | Owned job still awaiting completed cleanup. |
| `succeeded` | Natural exit 0, with exit code. |
| `failed` | Natural nonzero exit, or unavailable exact status/output; exit code when known. |
| `killed` | Explicit kill, close or cancelled unpublished start selected the cause. |
| `timed_out` | Automatic timeout selected the cause. |

Timeout omitted or zero uses `default_timeout`; a zero default disables it.
Capacity counts starting and hidden cleanup jobs until reaping. Finished records
retain bounded tails and are evicted oldest-completed-first only when admission
needs tracked space. `Limits::worst_case_*` helpers budget result escaping and
raw retained bytes. Tails and the registry are memory-only: after restart, old
IDs are not found. Paused pending starts may execute once on resume, while
Running calls and unsafe pending calls outside paused runs follow Crabber's
interrupted-recovery rule.

Rotate environment identity whenever effective environment values or policy
change. Values are excluded from hashes and Debug; `InheritAndOverride` freezes
the host environment once at construction. Changing shell identity, canonical
shell path, limits or other fingerprinted policy refuses strict paused resume.
Finish or settle runs before upgrading or rolling back.

Use a Tokio runtime with `enable_all()` and an I/O driver. Process support
installs a process-wide SIGCHLD handler. Closing any mount closes every job of
that instance; use separate instances for independent tenants. Interrupt active
runs before close. Shutdown has a bounded manager-close phase followed by a
separate bounded cleanup join; inspect `live_jobs()` for survivors. Host SIGKILL
can orphan commands and anchors, so hosts own external supervision.

Tests use synthetic commands in temporary directories. Set
`BACKGROUND_JOBS_REQUIRE_SHELL=1` to require `/bin/sh`, `ps` and `python3` instead
of allowing explicit fixture skips. CI requires these fixtures on Linux and
macOS. Non-Unix targets compile but reject construction as unsupported.
See [background-job parity differences](docs/extension-parity.md#background-jobs).

## Python REPL

`python_repl::PythonRepl` owns one host-provisioned Python 3.11–3.14 interpreter
per durable (session, workspace) pair. Calls on the same owner serialize through
a bounded FIFO queue; different owners can execute concurrently. Globals persist
between turns, including after Python exceptions, until a reset, clear or close.
The owner budget applies over the instance's lifetime, including failed setup.

```rust,ignore
use crabber_extensions::python_repl::{PythonRepl, Options, Environment, EnvironmentMode, Limits};
use std::{collections::BTreeMap, time::Duration};

let python = PythonRepl::new(Options {
    python_path: "/usr/bin/python3".into(), // host-provisioned; no discovery or installer
    python_identity: "host-python-v1".into(),
    temp_root: "/var/tmp/my-host".into(), // trusted, existing directory
    environment: Environment {
        mode: EnvironmentMode::ExplicitOnly,
        overrides: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        identity: "python-environment-v1".into(),
    },
    limits: Limits {
        max_sessions: 32,
        max_queued_per_session: 8,
        max_code_bytes: 65536,
        max_output_bytes_per_stream: 65536,
        max_result_bytes: 65536,
        max_exception_bytes: 16384,
        max_environment_entries: 128,
        max_environment_bytes: 65536,
        default_timeout: Duration::from_secs(30),
        max_timeout: Duration::from_secs(300),
        runner_start_timeout: Duration::from_secs(10),
        terminate_grace: Duration::from_millis(500),
        kill_wait: Duration::from_secs(5),
        shutdown_grace: Duration::from_secs(10),
    },
})?;
```

| Tool | Arguments | Permission metadata |
| --- | --- | --- |
| `python_repl` | Nonempty `code`; optional whole-second `timeout_seconds` (zero uses default) | `process.python.execute` |
| `python_repl_clear` | Empty object | `process.python.manage` |

Both tools are retry-unsafe. Permissions never grant execution: the host decides,
and `Ask` denies under `Agent`. Registration is atomic and construction never
executes Python. Use a Tokio runtime with process/I/O support (`enable_all()`);
Tokio installs a process-wide SIGCHLD handler. Linux and macOS execute; non-Unix
construction rejects with `unsupported-platform`.

Execute returns `status` (`completed` or `python_error`), bounded `stdout`,
`stderr`, trailing-expression `result` and trimmed `exception` fields, each
`{text, truncated}`, plus `generation`, `state_reset` and `state_reset_reason`.
Reset reasons are `canceled`, `timed_out`, `cleared` and `runner_failed` (empty
when no notice exists). A successful clear returns `{had_state, generation}`;
a stateful clear leaves a `cleared` notice for one subsequent execute. Clearing
an owner with no live runner consumes an older notice without recreating Python.
`Limits::worst_case_execute_bytes()` and `worst_case_clear_bytes()` help hosts
size inline result/snapshot budgets, including JSON escaping.

This is trusted Python with host-user authority, not a sandbox. There is no venv,
installer or implicit package provisioning. The interpreter uses `-I -u -B`:
host site-packages remain visible, bytecode writes are disabled, and the workspace
is absent from `sys.path` unless user code adds it. Each owner has mode-0700
`HOME`, five XDG directories and `TMPDIR`/`TMP`/`TEMP` under the trusted temp root.
Those nine keys are reserved in overrides; inherit mode replaces host values.
Python's standard input and user-visible fds 0/1/2 point at `/dev/null`; `input()`
raises EOFError. Captured Python writers carry bounded output. Trusted code can
still tamper with private protocol descriptors through `sys.modules`, OS APIs or
forking. Interpreter stderr is discarded; site hooks must stay silent because
output before bootstrap corrupts readiness. The runner source is visible in `ps`.

Crabber persists tool results; interpreter globals are memory-only. Rotate host
policy identities when interpreter or environment behavior changes, and settle
unfinished runs before adopting or reverting fingerprint changes. Environment
keys/values and temp root are excluded from the hash. Compose with
`ToolResultRedactor` to protect outward results before persistence; it does not
protect source code supplied in persisted tool arguments.

Cancellation and failed delivery reset state on tracked cleanup tasks. Close
interrupts startup/in-flight calls and terminates all owners concurrently under
one deadline, then removes private directories. The recommended cleanup defaults
above are TERM grace 500 ms, kill wait 5 s and shutdown grace 10 s; shutdown grace
must cover TERM grace plus kill wait. A TERM-honoring runner ends the grace early.
Inspect `live_runners()` after close: quarantined cleanup failures remain visible,
although Crabber shutdown returns Ok. Descendants that deliberately leave the
runner group are outside cleanup ownership. A killed host can leave orphaned
interpreters inside user code and stale private directories; hosts own that
cleanup. No parent-death watchdog or stale-directory sweep is provided. Host
SIGCHLD auto-reaping or `waitpid(-1)` is outside the contract.

Tests use synthetic data and provisioned Python; `PYTHON_REPL_REQUIRE_PYTHON=1`
forbids interpreter skips and requires 3.11–3.14. CI selects 3.11 and 3.14 on
Linux/macOS. See [the parity boundaries](docs/extension-parity.md#python-repl).
Full nine-feature parity is not claimed.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
```

CI defines Linux and macOS gates and fetches the pinned Crabber source over
HTTPS without a repository access token. Rust is pinned in
`rust-toolchain.toml`. Changing frozen extension policy changes the run-plan
fingerprint; finish or settle unfinished runs before adoption or rollback.
No data migration is introduced.
