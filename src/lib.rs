// pub mod diff;
// pub mod fsmonitor;
pub mod index;
// pub mod object;

use std::result;

pub type Result<T> = result::Result<T, anyhow::Error>;

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub struct Oid([u8; 20]);

impl Oid {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut oid_bytes = [0u8; 20];
        oid_bytes.copy_from_slice(bytes);
        Oid(oid_bytes)
    }
}
