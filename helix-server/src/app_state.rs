use crate::storage::storage::{FsObjectStore, FsRefStore};

#[derive(Clone)]
pub struct AppState {
    pub objects: FsObjectStore,
    pub refs: FsRefStore,
}
