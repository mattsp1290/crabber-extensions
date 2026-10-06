# Native extension parity

Reference: `eino-agent-extensions` commit
`5389549b1f156013a0f82bc936ce4f10edb1ce9f`, using Eino Agent v0.3.3.
Crabber is pinned to published main revision
`6c59b01103849bde1179a6f0ea818c5a7b516820`.
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
| Background jobs | Bounded session/workspace process ownership, then atomic tool registration and durable journey | `crabber-extensions-nasz`, `crabber-extensions-477p` |
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
approver and store see task and profile. Crabber's schema accepts NUL; the
extension itself rejects NUL in both fields. Valid runner response text is
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
Crabber's schema accepts NUL, so the extension rejects NUL queries itself.
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

## Verification and limits

Local gates: formatting, Clippy, tests and documentation build. CI runs the
same gates on Linux and macOS. The repository requires a `CRABBER_READ_TOKEN`
Actions secret with read access to the private Crabber repository; a normal
repository GITHUB_TOKEN cannot read that sibling. Hosted CI results and macOS
execution are not claimed merely because local Linux gates pass.

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
