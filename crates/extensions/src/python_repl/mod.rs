//! Owner-scoped Python execution for trusted host workloads.
#![cfg_attr(not(test), allow(dead_code, unused_imports))]
mod runner;
pub use runner::BoundedText;
#[cfg(all(test, unix))]
mod tests;
