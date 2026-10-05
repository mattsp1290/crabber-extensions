# Working in crabber-extensions

This Rust 2024 workspace ports the nine native features in eino-agent-extensions.
Read docs/extension-parity.md for scope and tracked work. Use bn for project
issues and plan crabber-extensions-plan-pygp. Preserve dependency edges and
record the implementing commit when closing work.

Run cargo fmt --all --check, cargo clippy --workspace --all-targets --locked --
-D warnings, cargo test --workspace --locked, and RUSTDOCFLAGS='-D warnings'
cargo doc --workspace --no-deps --locked after code changes.

Consume only Crabber public APIs from the immutable Git pin. Do not replace it
with a development path override in manifests or committed Cargo configuration.
Trust, credentials, UI, subprocess provisioning and policy belong to the host.
Tests use scripted providers and synthetic data, without provider credentials.
Keep .agents/ and reviews/ local and ignored. Do not claim complete parity while
features remain planned or substitute pattern matching for shell analysis.
