//! Digest-verified host-provisioned RTK tool-result reduction.
#![cfg_attr(not(test), allow(dead_code, unused_imports))]

mod child;
mod config;
mod engine;
mod private_dirs;
#[cfg(all(test, unix))]
mod tests;

pub use config::{
    Binding, DEFAULT_ORDER, Field, Filter, InputMatch, Limits, Options, RTK_VERSION, Selection,
};
