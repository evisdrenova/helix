pub trait ObjectStore {
    fn has_object(&self, ty: ObjectType, hash: &[u8; 32]) -> bool;
    fn write_object(&self, ty: ObjectType, hash: &[u8; 32], data: &[u8]) -> anyhow::Result<()>;
    fn read_object(&self, ty: ObjectType, hash: &[u8; 32]) -> anyhow::Result<Vec<u8>>;
}

pub trait RefStore {
    fn get_ref(&self, name: &str) -> anyhow::Result<Option<[u8; 32]>>;
    fn set_ref_if_matches(
        &self,
        name: &str,
        expected: [u8; 32],
        new: [u8; 32],
    ) -> anyhow::Result<Result<(), RefUpdateError>>;
}

pub enum RefUpdateError {
    NotFastForward,
    UnexpectedCurrent([u8; 32]),
}
