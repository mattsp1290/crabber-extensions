# External durable-host consumer

Run from a checkout containing the revisions being qualified:

```sh
EXTENSIONS_REV=<pushed-full-sha> ./testdata/external-consumer/run.sh
TOOLS_REV=<compatible-tools-full-sha> EXTENSIONS_REV=<pushed-full-sha> \
  ./testdata/external-consumer/run.sh
```

The script creates an empty project outside the repository. Set
`CRABBER_EXTENSIONS_CONSUMER_DIR` to select an empty external destination and
`CRABBER_EXTENSIONS_EVIDENCE_DIR` to copy its logs, lockfile, lock hash, metadata
and dependency trees to an evidence directory. Requires Git, Python 3.11–3.14,
Rust 1.99.0, `/bin/sh`, `ps`, and `rg`; no provider credentials are used.

Cargo sees only immutable Git dependencies for Crabber, extensions and optional
standard tools, with the same SSH source spelling. HTTPS fetch rewrites do not
change Cargo identity. No development path override is used. The consumer has its
own workspace. It generates a lock once, then uses `--locked` for all builds,
runs and dependency inspection; no lockfile for substituted revisions is checked
into this template.

The dedicated binary exercises a configured host and SQLite restart. To qualify
the full durable-input, attribution, isolation and cleanup edge cases without
maintaining a second copy, the script exports six public-API integration targets
and their fixtures from the **same immutable extension commit** using Git
archive. They compile afresh against the consumer's Git dependencies, outside
the original workspace. These are reused public-contract journeys, not an
independent implementation or internal-library tests. No private source module
or path dependency is imported. Both stages run this external test harness.

Stage 1 pins extensions plus Crabber. Stage 2 adds a restricted standard-tools
catalog only when TOOLS_REV uses the identical Crabber revision and URL. The
identity check requires exactly one version/source identity for every Crabber
tuple package and the expected six packages sharing the exact public pin. It
checks duplicate roots rather than rejecting Crabber appearing as a dependent
beneath another duplicated package.

Restart runs in two actual processes. Phase 1 commits an active detached owner
and a paused ask, then exits without graceful cleanup to simulate host loss.
Phase 2 answers and continues the ask with explicit replay checks, observes the
old owner still Active, and verifies its fresh local drain/cache cannot claim a
join or resume an old process. This proves neither provider behavior nor exactly
once side effects. Live provider and macOS qualification remain separate.
