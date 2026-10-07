//! Bounded, tracked child lifecycles for trusted native extensions.
#![cfg_attr(not(test), allow(dead_code, unused_imports))]

mod script;
mod tail;
#[cfg(unix)]
mod unix;
#[cfg(not(unix))]
mod unsupported;

pub(crate) use script::{SUPERVISOR_PROTOCOL, supervisor_digest};
pub(crate) use tail::Tail;
#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(not(unix))]
pub(crate) use unsupported::*;

#[cfg(all(test, unix))]
mod tests;
