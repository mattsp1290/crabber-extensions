# Native extension parity

Reference: `eino-agent-extensions` commit
`5389549b1f156013a0f82bc936ce4f10edb1ce9f`, using Eino Agent v0.3.3.
Crabber is pinned to published main revision
`883189465397b2fd326a6ef358c8a2fbd39fbec8`.
Requests `crabber-r-m5pf`, `crabber-r-u7l3`, and `crabber-r-0f7c` are resolved
with consumer acceptance recorded in Beans.

The canonical execution plan is `crabber-extensions-plan-pygp`, with rollup
`crabber-extensions-6ynl`. Use `bn plan show crabber-extensions-plan-pygp` and
`bn ready` to inspect current execution state. Local editable planning files
live under ignored `.agents/plans/extension-parity/`; Beans holds the shared copy.

| Feature | Scope | Work |
| --- | --- | --- |
| Workspace instructions | Trusted host resolver, bounded fresh boundary-to-root file reads, named per-attempt contribution | `crabber-extensions-0hzm` |
| Tool result redactor | Built-in/custom patterns, exact exclusions, bounded JSON traversal, final-redaction phase | `crabber-extensions-ux3i` |
| Ask user | Bounded host-mediated request/reply; explicit permissions and cancellation | `crabber-extensions-isy8` |
| Delegated tasks | Host-owned child runner, authoritative parent/workspace context and tracked cleanup | `crabber-extensions-fvhv` |
| Web search | Host-owned backend callback; bounded queries/results and sanitized failure | `crabber-extensions-g6mu` |
| Command guard | Bounded syntax analysis and conservative wrapper/dialect policy, then runtime integration | `crabber-extensions-iodk`, `crabber-extensions-jlr5` |
| Background jobs | Bounded session/workspace process ownership, four atomic tools and durable journeys | `crabber-extensions-nasz`, `crabber-extensions-477p` |
| Python REPL | Session-scoped interpreter lifecycle, then REPL/clear tools and resume/reset proofs | `crabber-extensions-b6qy`, `crabber-extensions-ajk3` |
| RTK reducer | Explicit executable verification/bindings, then cancellation/reaping and redactor composition | `crabber-extensions-rja2`, `crabber-extensions-9srp` |

Infrastructure is `crabber-extensions-iaa6`; shared process lifecycle work is
`crabber-extensions-gurf`; final nine-feature acceptance is
`crabber-extensions-2bed`.

## Operating decisions

One library crate starts the workspace. Feature modules have separate options,
configuration hashes and registration surfaces. Further crate splits should
follow actual dependencies rather than anticipating abstractions. There are
no active consumers requiring compatibility shims or feature flags.

Hosts own UI, trust, process policy, executable artifacts, credentials and
adapter lifecycle. Native extensions are trusted code. The library does not
bundle providers, installers, shell tools or network backends. Crabber owns
identity, fencing, dispatch, cancellation precedence and durable settlement.

Configuration hashes cover behavior-bearing limits, patterns, names, order
and resolver identity. File contents are deliberately dynamic. Change the
host resolver identity when its trust/routing policy changes. Mounts have
Crabber's frozen-plan fingerprint and strict resume semantics.

## Execution and acceptance

1. Bootstrap the pinned workspace and Linux/macOS verification workflow.
2. Deliver workspace instructions and final result redaction with public-facade
   tests, including protected durable messages/events/results and next model input.
3. Port the three host callback bridges and command policy.
4. Build bounded process ownership and port jobs, Python and RTK in small tasks.
5. Verify nine-feature composition, permissions, session isolation, resume,
   cancellation, cleanup and fingerprint changes. Document representation and
   platform differences before claiming full parity.

Crabber uses `serde_json::Value` tool results rather than Eino's raw JSON,
metadata and attachment envelope. This redactor scans JSON strings and keys;
it does not create an Eino envelope or rewrite durable inputs/call identities.
Instruction rendering uses an explicit Crabber format version and smaller
bounds compatible with Crabber's 32 KiB contribution limit.

The ask-user callback is an awaited, droppable future: deadline and interruption
drop it and free capacity immediately, unlike the reference implementation which
holds its slot until the responder returns. Dropping is its only cancellation
signal; requests carry no cancellation token, responders must not block while
polled, and hosts own work they spawn. `required_permissions` is metadata and
the host `PermissionPolicy` decides access; `Ask` denies under `crabber::Agent`.
The extension declares `retry_safe: false`, so Crabber recovery interrupts a
pending call rather than prompting once; run-level recovery proof remains under
`crabber-extensions-2bed`. Mount close does not cancel an active ask or make a
later call unavailable, so hosts interrupt runs before closing. Guards, policy,
approver and store see questions and options; Crabber rejects NUL-bearing model
input before the extension. Custom answers are stored verbatim and hosts needing
redaction mount the result redactor. Parsed JSON resolves duplicate keys and lone
surrogates before this extension; option indexes in errors and `allow_custom` are
omitted. Its configuration hash is not the Go hash.

Delegated tasks run on an extension-owned tracked task. Requests carry an
explicit cancellation token that fires on run interruption, the wait deadline
and every executor exit. Capacity remains held until the runner future exits,
as in the reference, so timed-out and interrupted calls can settle while their
runner winds down. Hosts must observe the token, return promptly, and avoid
blocking during polling. A runner may first poll with an already-cancelled
token and must check it before starting side effects. The extension creates no
child agents, sandbox, providers or credentials; `profile` is opaque host
routing data. Use one instance per registry or tenant: shared instances share
capacity and closing one mount waits for every mount's runners.

Shutdown waits at most `shutdown_grace`, then silently detaches stragglers;
they retain their slots until exit. Mount close can take its registry drain
bound plus this grace; the second timeout is not reported as
`MountCloseTimeout`. This grace replaces the reference's caller-supplied Close
deadline. Shutdown does not cancel still-polled executors; hosts interrupt
active runs before closing. Results are bounded per call by `max_result_bytes`.
`max_in_flight` bounds simultaneous live runners, not results accumulated in a
turn or conversation. Completed calls release slots for further calls in the
same turn. Hosts size result bounds with their tool-call and context budgets;
JSON escaping can expand each encoded result.

Delegation's `required_permissions` is metadata; host `PermissionPolicy` decides
access and `Ask` denies under `crabber::Agent`. Go's
`delegate-profile:<profile>` approval pattern has no equivalent; hosts read the
profile argument in their policy. `retry_safe: false` is declared so Crabber
recovery interrupts pending calls rather than re-running them; run-level
recovery proof remains under `crabber-extensions-2bed`. Guards, policy,
approver and store see admitted task and profile. The pinned Crabber runtime
rejects NUL-bearing durable arguments before execution; direct executor calls
also reject NUL in both fields. Valid runner response text is
host-trusted and stored verbatim within the result bound. Runner errors,
panics and invalid output become one sanitized failure; error payloads are
discarded, so host-visible failure text belongs in `failed` or `rejected`
responses. Hosts needing redaction mount the result redactor. There is no
retention policy. Parsed JSON resolves duplicate keys and lone surrogates
before the extension sees input; the configuration hash is not the Go hash.

Web search awaits a drop-safe host searcher inline, as ask-user does. Deadline,
interruption and a settled reply drop the future and release capacity immediately;
the reference holds its slot until the backend goroutine returns. Drop is the
only cancellation signal. Hosts must not block while polled, own spawned work,
and supply network access, credentials, rate limits, caching and freshness.
`max_in_flight` bounds awaited futures only. Capacity exhaustion, deadline and
a reply at or after the deadline are fixed tool failures (`capacity`,
`timed_out`); backend errors and call/poll panics become `searcher`. Panic in
future destruction is contained and preserves the selected outcome. Invalid
source records are dropped; an all-invalid reply is an empty success.

Queries are trimmed before the byte bound and callback delivery. Durable call
arguments retain the model's raw text. There is no `max_raw_input_bytes`;
parsed JSON resolves duplicate keys and lone surrogates before the extension.
The pinned Crabber runtime rejects NUL-bearing durable arguments before execution;
the extension also rejects NUL queries when called directly.
Only the first `max_results` sources are inspected, without refilling invalid
records. Titles and snippets are truncated at a UTF-8 boundary; URLs are stored
verbatim without repair. NUL in a title or snippet drops that record, whereas
the reference passes it through. Rust strings cannot represent invalid UTF-8.

URL validation uses the WHATWG `url` parser behind raw guards for control bytes,
exact lowercase HTTP(S) schemes, nonempty authorities without userinfo, the
reference's authority alphabet excluding all percent escapes, and valid percent
escapes everywhere. URLs are never re-serialized. The executed Go-reference
corpus agrees or is stricter on every tested row; this is no universal claim.
Stricter cases include percent escapes in authorities and invalid query escapes,
`<` and `>` and a mid-host `]`, ports above 65535, IPv4-like hosts with too many
parts, IPv6 zone identifiers, and IDNA compatibility mapping such as U+00A0.
The dependency adds 26 lock packages under MIT, Apache-2.0 and Unicode-3.0.

Search requests add authoritative session, run and call identity and workspace
routing data absent from the reference callback. Bounds apply to each persisted
result and cannot prevent host vector/string allocations. There is no retention
policy; `Limits::worst_case_result_bytes()` includes worst-case JSON escaping.
Parallel execution can carry `max_in_flight` such results, and subsequent calls
in a turn can accumulate more after capacity frees. Permission metadata is
`network.web.search`; host `PermissionPolicy` decides access and `Ask` denies
under `crabber::Agent`. Go's `Pattern` hook has no equivalent. `retry_safe: false`
interrupts pending calls on recovery rather than resuming them once; run-level
proof and redactor composition remain under `crabber-extensions-2bed`. The hash
is not the Go hash and covers limits and backend identity, never the closure.
Hosts must change that identity when backend routing or behavior changes.
Shared instances share capacity; hosts interrupt active runs before closing.

## Command syntax analysis

`command_guard::Policy` delivers the pure analysis library for
`crabber-extensions-iodk`. Hosts provide explicit tool/command-field/dialect
bindings, basename and positional-prefix deny rules, and fourteen positive
limits. `Policy::analyze` takes a parsed JSON object; `analyze_script` takes a
script directly. Both return fixed outcomes without retaining input or touching
processes, files, environment, network or credentials. `Abstain` is never an
approval. `CommandGuard` registers an inline synchronous Crabber `ToolGuard`, takes
atomic capacity permits from `Limits::max_in_flight`, contains unwinding analysis
panics with `catch_unwind`, and exposes per-class counters. Denials persist as
Crabber's fixed `permission denied`. Every check blocks its Tokio worker for the
analysis duration (up to the two-second debug worst case); capacity denial does
not unblock a worker already analyzing. Hosts use multi-thread runtimes and keep
`max_in_flight` at or below the workers they can afford to block.

The parser is hand-written and fail-closed, with one context-sensitive lexer
owned by a recursive-descent parser and a postorder AST walker. No dependency,
manifest, lockfile, Crabber pin or CI change is needed. The user selected this
approach after testing brush-parser 0.4.0: 2,000 nested substitutions overflowed
a 2 MiB stack; its word parsing also uses a global cache and its normal
dependencies add 86 lock packages. Tree-sitter-bash requires a C build and has a
Bash-only, error-tolerant grammar; yash-syntax is GPL-3.0-or-later. The measured
Rust hard depth cap is **32**, lowered from the proposed 128 after a debug
analysis overflowed a 512 KiB thread at 128. Hard-cap nesting, wrapper,
backquote, shell and large-word/case generators now return on that thread.
Depth is shared by syntax and wrappers, and bytes/nodes/words are shared by
nested scripts. Tests require each worst-case analysis to finish within two
seconds in debug builds, including 256 rules comparing 64-token prefixes.

The source reference is still `eino-agent-extensions` at
`5389549b1f156013a0f82bc936ce4f10edb1ce9f`, with mvdan shell parser v3.14.1.
The differences below are explicit analysis boundaries; every fixed outcome
except abstention denies. The generated reference corpus verifies all rows
agree or deny more conservatively, with each stricter row tied to a key.

| Key | Analysis behavior and difference |
| --- | --- |
| `parser` | An AST for the admitted POSIX/Bash grammar replaces the full mvdan parser and walker. Functions, arithmetic, arrays, extended tests, `time`, `coproc`, `select` and other unsupported constructs deny. |
| `json-bytes` | Aggregate key and string bytes are bounded by `max_analysis_bytes` before NUL scans. This is a separate extraction budget; parsed script byte accounting remains shared across nested scripts. The plan's node-only sibling walk otherwise permitted unbounded scanning of one giant JSON string. |
| `raw-input` | Parsed `serde_json::Value` has no raw-input bound or duplicate-key/invalid-UTF-8 channel. All sibling values and keys charge JSON budgets. Sorted key order can change the first denial class for a multi-fault object. |
| `bindings` | Bindings are explicit; an empty vector is an error. `default_bindings()` supplies `shell/cmd/posix` and `background_job_start/command/posix`. Keys are literal, including periods. |
| `limits` | Fourteen limits replace fifteen; no raw-input limit. Hard caps and AST/word/depth accounting are library-defined; byte budgets include re-parsed backquotes and nested shells. |
| `outcomes` | Five fixed analysis outcomes and `Outcome::code()`. Runtime denials persist Crabber's fixed `permission denied`; counters replace the Go message and capacity code. |
| `hash` | A canonical Rust tuple includes versioned behavior identity, every binding, rule and limit. Binding/rule order is ignored. Configuration and corpus-outcome digests are pinned. It is not the Go hash. |
| `identifiers` | 1–256 ASCII alphanumerics, `_`, `-`, `.`, `:`; dots alone are permitted by this exact alphabet. Executable basenames may not be empty, `.`, `..`, or contain slash, backslash or NUL. |
| `cr` | CR remains an ordinary word byte; no mask around the parser is needed. |
| `comment-continuation` | A comment ends at its first newline even after backslash. The reference abstains on `echo a # c \<LF>blocked`; dash and Bash execute `blocked`, and Rust returns `RuleMatch`. Escaped backslashes do not erase a following newline. |
| `heredoc-in-substitution` | Heredocs inside command/process substitutions or re-parsed backquotes are unanalysable. |
| `heredoc-line` | Multiline quotes or substitutions on a pending heredoc operator's line are unanalysable. |
| `heredoc-continuation` | Unquoted heredoc backslash-newline groups are unanalysable because Bash and dash disagree on joined closing delimiters; quoted heredoc bodies remain literal data. |
| `heredoc-delimiter` | Expansions in the delimiter are unanalysable instead of a reference parse error. Quoted bodies are data; unquoted bodies use the same lexer; `<<-` strips leading tabs before parsing even within quotes. |
| `brace-expansion` | Unquoted literal parts containing `{` are unknown. Inspection of pinned `SplitBraces` showed it returns true even for unmatched or escaped braces, contrary to the plan's description; preserving this behavior prevents weaker verdicts. Quoted braces remain known. |
| `posix-ansi-quote` | ANSI and locale quote parts are unknown in both dialects. The reference treats POSIX `$'..'` as literal text and can miss `$'blocked'` or `git $'push'` executed by Bash-as-sh. |
| `ansi-quote-backslash` | A backslash before the first closing ANSI quote denies in both dialects, because Bash and dash disagree about where that quote ends. |
| `reserved-out-of-place` | Reserved words the grammar cannot consume in command position deny, including a second `!` and out-of-place `in`. |
| `posix-time-coproc` | Both are unanalysable in both dialects; the reference's POSIX abstention on `time blocked` misses execution by dash and Bash-as-sh. |
| `posix-arith-subshell` | Adjacent `((` in command position denies under either dialect, including the POSIX ambiguity with nested subshells. Spaced `( (` remains supported. |
| `posix-extended-test` | `[[` denies under either dialect, closing the reference POSIX ordinary-word interpretation. |
| `posix-bracket-arith` | `$[` denies under either dialect; Bash-as-sh evaluates it arithmetically while the reference POSIX parser treats it as literal. |
| `posix-append` | Append assignments deny in both dialects instead of being ordinary words under the reference's POSIX parser. |
| `select` | Bash `select` denies instead of walking a for-clause. |
| `fd-variable-redirect` | Adjacent `{name}>`/`{name}<` variable descriptor forms deny. |
| `deny-class` | POSIX process substitution and pipe-amp are invalid; other Bash-only redirect operators are unanalysable. Different denial classes all deny. |
| `extglob` | Extended-glob openers deny as unanalysable under either dialect. |
| `redirect-target-fd` | A numeric redirect target immediately followed by another redirect operator denies. Reference abstentions such as `2>&1>>file` fail real-shell syntax checks in dash and Bash. |
| `posix-indexed-word` | Incomplete command-position `NAME[` words deny in both dialects; Bash-as-sh rejects forms the reference's POSIX parser treats as ordinary commands. |
| `cancellation` | Analysis is synchronous and bounded; no cooperative context cancellation. |
| `capacity` | Atomic permits are taken only for bound tools; saturation denies immediately and is counted. Checks never overlap within one run task, including parallel execution mode; capacity matters across runs or threads. Each check blocks its worker; size the limit at or below the workers the host can afford to block. |
| `internal` | Analysis panics are caught, counted and denied; the reference returns an internal error that fails the call. Stack overflow, allocation failure and `panic = "abort"` abort. Hard-cap runtime proofs passed in debug Linux on default 2 MiB and explicit 1 MiB worker stacks; they do not cover current-thread runtimes or arbitrary host-sized stacks. |
| `diagnostics` | No per-call diagnostic message is added. `Stats` and host re-analysis replace Go's code/message; capacity/internal are counters only. The host-owned panic hook may print a caught panic's payload before denial, unlike Go recovery. The user accepted this deviation on 2026-10-06. Module lints and the zero-hit G1 audit of text-bearing panicking std calls support sanitization, without a hook-output test or universal proof. Counter snapshots read fields independently and individual counters wrap at `u64::MAX`. |
| `scope-order` | Scope is the host's Crabber mount argument and is not fingerprinted; guards have no order and all run. Go scope/order configuration and duplicate-mount rejection have no equivalent. Shared instances share counters and capacity across mounts and registries. |
| `resume` | Pending calls of paused runs are guarded again on stored normalized arguments without rerunning `ToolPrepare`. Any rule, binding or limit drift refuses resume with `PlanChanged` before mutation. Crabber marks calls Running before guards; a crash during analysis settles Interrupted on recovery without re-guarding (D10), although the executor never ran. `recover()` sweeps and Running-call settlement proofs remain `crabber-extensions-2bed`. |
| `resume-scope` | A different mount scope with equal options resumes a paused run; the reference rejects scope/order drift. |
| `guard-layer` | Delivered deny-only `CommandGuard` registers one guard, with a versioned wrapper hash over policy identity; no tools or shutdown resources. |

Basename/prefix rules do not match options before a subcommand (`git -C . push`),
`nohup`, `nice`, `ionice`, `xargs`, `find -exec`, `busybox`, `ssh`, `python -c`,
`perl -e`, `node -e`, `make`, host-defined shell functions or aliases, or
executables with a different basename. This is trusted syntax inspection, not a
sandbox. Hosts own execution environment, provisioning, trust and permissions.

## Background jobs

`background_jobs::BackgroundJobs` delivers bounded process ownership and the
four start/status/list/kill tools. The host supplies a canonical executable
shell, shell identity, frozen environment policy and twelve Limits fields.
Permission metadata never grants execution; the host PermissionPolicy decides.
All commands and retained output are trusted host workloads, not sandboxed data.
The process module remains private and shared with future process consumers.

| Key | Reference behavior and Rust boundary |
| --- | --- |
| `supervisor` | The reference uses status/readiness/gate FDs 3/4/5. Under `unsafe_code = "forbid"`, Rust uses a fixed POSIX supervisor, stdin `G` gate, exit status and a `command -p sleep` group anchor. No readiness byte or extra descriptors; exec failure is `spawn-failed`, gate failure is `gate`. Shell `128+n` cannot distinguish a signal from explicit exit of that value. The anchor preserves the group ID during normal cleanup; externally killing an unanchored leader leaves a narrow group-ID reuse window. Host SIGKILL can orphan commands/anchors. |
| `host-subprocess` | Crabber's public Subprocess service is one-shot without streaming/group/kill contracts, so jobs use safe Tokio process APIs and rustix signals directly. Hosts still own subprocess provisioning and policy. |
| `workspace-root` | Initial root is authoritative WorkspaceContext.directory verbatim, canonicalized at start. Relative working directories must resolve beneath it. This replaces Eino WorkspaceRoot routing; it constrains launch only. |
| `retention` | Memory-only raw-byte stdout/stderr tails and tracked terminal records replace Eino retention/transcript handling. Saturating worst_case_status/list/retained_bytes helpers include result escaping or raw retention as documented; maximum raw tails reach 512 MiB at hard caps. No spill or retrieval API. |
| `limits` | Twelve explicit bounds have library hard caps; max_running≤64, max_tracked≤256, command≤64 KiB, relative directory≤4 KiB, each tail≤1 MiB, environment≤1024 entries/256 KiB. Timeout max is 24 hours; default zero disables it. TERM grace and kill_wait are positive ≤60s; added shutdown_grace is positive ≤120s and applies separately to close and join. |
| `hash` | Versioned Rust hash, not the Go hash. Includes tool names, all permission identifiers, canonical shell OS bytes, shell identity, supervisor protocol/digest, every limit, environment mode/identity. Environment values are excluded; hosts rotate identity when values or policy change. Non-UTF-8 Unix paths hash losslessly. |
| `timestamps` | UTC RFC3339 with nine fractional digits replaces the reference encoding. Civil-date tests pin epoch, leap day, negative time and dates beyond 2038. |
| `environment-utf8` | ExplicitOnly passes only overrides. InheritAndOverride freezes the host environment once, then applies overrides. Non-UTF-8 inherited keys/values, NUL and invalid keys reject construction. Debug omits keys/values and shell paths. |
| `recovery` | Crabber D10 governs pending/running calls: paused pending start may execute once on resume; Running calls and unsafe pending calls outside Paused settle Interrupted. Start/kill retry_safe=false, status/list=true. Registry/output are process-local, so fresh instances see job-not-found/empty lists. recover() sweep proofs remain 2bed. |
| `permissions` | Constant metadata is background.process.start/read/kill; host policy owns access and Ask denies under Agent. Denied starts have PermissionDenied with fixed permission denied and do not spawn. No Eino approval pattern adapter. |
| `cancellation` | A tracked start task owns the reservation, child and publish decision. Cancellation before publication withholds the launch gate; gate failure retains hidden cleanup ownership until reaping. Dropping a live-token caller may still publish. Call cancellation does not kill published jobs. Accepted termination continues after the kill caller cancels, with first-cause precedence and persisted escalation phases. |
| `close` | Closing one mount closes the shared instance. Manager close waits for starts, requests termination and joins completed jobs under its bound; cleanup joins under a second bound. Crabber shutdown itself is unbounded, and mount close returns Ok once shutdown returns even when jobs survive. live_jobs() exposes remaining starting/running/hidden work. Interrupt runs before closing; finished jobs never block close. |
| `runtime` | Requires Tokio process/I/O support and a runtime such as Builder.enable_all(). Tokio installs a process-wide SIGCHLD handler. Real-child tests use multi-thread runtimes and real clocks. |
| `platform` | Linux and macOS execute POSIX jobs and group signals. Non-Unix builds compile but BackgroundJobs::new rejects unsupported-platform. No shell discovery or Windows runner. |
| `inputs` | Parsed JSON accepts only exact tool fields, typed values, bounded UTF-8 bytes and lowercase 53-byte IDs. null timeout is rejected by schema; zero/omitted uses default. Parsed duplicate keys/surrogates are serde_json's boundary. The newer Crabber pin can reject NUL before execution; direct tests preserve the extension's own validation contract. |
| `dependencies` | Tokio adds process/io-util; Unix rustix adds process, without unsafe calls. Lock additions: mio (MIT), signal-hook-registry (Apache-2.0 OR MIT), wasi (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT). No shell/parser/runtime discovery dependency. |
| `scope` | Owner is durable session plus workspace ID; other owners cannot read/list/kill a job. Shared mounts share running/tracked capacity and closing. Finished records are pruned oldest-completed-first only for admission, never evicting running jobs. List sorts started_at then ID. |

Configuration and internal seam tests cover every limit/hash input, group
escalation and reaping retries, publication cancellation/drop, hidden cleanup,
coalesced kills and timestamp boundaries. Public tests cover exact registration,
atomic collision rollback, session visibility, durable values/events/model input,
permission classes, directory containment, input/output bounds, all lifecycle
paths, an eighteen-row paused-resume drift matrix and unchanged persistence after
refusal. Redactor and command-guard composition protect outward results while
preserving in-memory output. They do not redact persisted command arguments;
marker fixtures therefore supply synthetic values through the environment.

Strict fixture execution requires `/bin/sh`, `ps` and `python3`; the two detached
output-holder tests publish PID ownership before detaching and readiness after
setsid, and verify holder disappearance after cleanup. CI sets
BACKGROUND_JOBS_REQUIRE_SHELL=1 on Linux/macOS. Host-process restart and nine-feature
composition acceptance remain separate work; this is not complete parity.

## Verification and limits

Local gates: formatting, Clippy, tests and documentation build. CI runs the
same gates on Linux and macOS. CI fetches the pinned Crabber source over HTTPS
without a repository access token, so the source must be publicly readable.
Hosted CI results and macOS execution are not claimed merely because local
Linux gates pass.

The initial slice tests regular/missing/invalid/oversized files, boundary order,
symlink exclusion, pinned directory handles, resolver errors, live refresh,
concurrent workspace isolation, session shadowing and tracked deadline cleanup,
JSON bounds, token boundaries, pattern validation, identity hashes, composition
and protected persistence. Further feature tasks remain open until their own
contracts and combined acceptance pass.

Ask-user tests cover configuration and schema metadata, selected/custom/dismissed/
unavailable/timed-out outcomes, permission denial, sanitized responder failures,
input validation, cancellation, capacity release, and interrupt cleanup.

Delegated-task tests cover configuration, authoritative request/workspace routing,
durable outcomes, permissions, sanitized faults, input and output bounds,
parallel capacity, cancellation, slot retention until exit, bounded shutdown,
shared mounts, and host-owned child-agent completion and interruption.

Web-search tests cover configuration and fingerprints, persisted runtime results,
permissions, input byte bounds, sanitized faults, URL reference verdicts and
field bounds, cancellation/deadline/drop/capacity lifecycle, and session routing.

Command-analysis suites cover contract, input, reference corpus, differential,
shell, budgets and robustness. The [fixture reproduction instructions](../crates/extensions/tests/command_guard/fixtures/README.md)
describe the overlay generation of 599 original expanded cases and the 10,888-row
Go differential corpus. The Unix suite checks accepted syntax, exact argv and
synthetic execution canaries, and skips loudly if Bash 5+ is absent or `/bin/sh`
is an older Bash. `COMMAND_GUARD_REQUIRE_SHELLS=1` forbids skipping. The PS4
implicit-execution proof uses Bash 5 as `sh`; this machine's dash does not expand
that inherited external substitution. The 512 KiB debug stack and two-second
wall-clock checks are local Linux evidence, not macOS or hosted-CI results.

Command guard integration adds deterministic unit seams for saturation and panic,
public registration/contract and concurrency checks, durable runtime journeys,
a 22-row paused-resume drift matrix and a Unix `/bin/sh` canary. Hard-cap Agent
proofs passed locally in Linux debug builds on 2 MiB and 1 MiB workers. Two plan
expectations were corrected against the immutable implementation: exact-limit
word fixtures abstain, and a two-call turn prepares twice before pause, zero
times on resume. The depth-31 admitted substitution matches a deny rule; depth
32 exhausts the depth budget. Full nine-feature composition remains open.
