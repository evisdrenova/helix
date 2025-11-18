// pub mod diff;
// pub mod fsmonitor;
pub mod index;
// pub mod object;

use std::result;

pub type Result<T> = result::Result<T, anyhow::Error>;

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub struct Oid([u8; 20]);
