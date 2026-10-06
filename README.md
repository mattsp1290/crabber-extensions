# Crabber Extensions

Trusted native extensions for [Crabber](https://github.com/mattsp1290/crabber).
The first implementation slice provides workspace instructions, bounded
ask-user interaction, bounded delegated tasks, bounded web search, and final
JSON tool-result redaction, plus bounded command syntax analysis (guard
integration pending). Four additional features remain planned in
[the nine-feature parity plan](docs/extension-parity.md).

The workspace consumes only published Crabber public APIs, pinned to
`6c59b01103849bde1179a6f0ea818c5a7b516820`. Fetching the private dependency
requires existing Git read access. Tests and the example use scripted providers
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
filesystem, environment or network access occurs during analysis. The Crabber
`ToolGuard` wrapper arrives with `crabber-extensions-jlr5`; `max_in_flight` is
reserved for that integration.

This is trusted syntax inspection, not a sandbox. Known non-matches include
`git -C . push`, `nohup`, `nice`, `ionice`, `xargs`, `find -exec`, `busybox`, `ssh`,
`python -c`, `perl -e`, `node -e`, `make`, host-defined shell functions/aliases,
and different executable basenames. See [parity differences](docs/extension-parity.md#command-syntax-analysis)
and the [reference fixture procedure](crates/extensions/tests/command_guard/fixtures/README.md).
Unix shell tests require Bash 5+; set `COMMAND_GUARD_REQUIRE_SHELLS=1` to require
execution instead of an explicit skip. CI configuration remains a host decision.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
```

CI defines Linux and macOS gates. Configure `CRABBER_READ_TOKEN` in Actions with
read access to the private Crabber repository. Rust is pinned in
`rust-toolchain.toml`. Changing frozen extension policy changes the run-plan
fingerprint; finish or settle unfinished runs before adoption or rollback.
No data migration is introduced.
