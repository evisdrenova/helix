use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub type Hash32 = [u8; 32];

#[derive(Debug, Serialize, Deserialize)]
pub enum RpcMessage {
    Hello(Hello),
    HelloAck(HelloAck),

    PushRequest(PushRequest),
    PushObject(PushObject),
    PushDone,
    PushAck(PushAck),

    FetchRequest(FetchRequest),
    FetchObject(FetchObject),
    FetchDone,
    FetchAck(FetchAck),

    Error(RpcError),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub client_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HelloAck {
    pub server_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushRequest {
    pub repo: String,       // just repo name or path
    pub ref_name: String,   // "refs/heads/main"
    pub old_target: Hash32, // ZERO_HASH for "no remote yet"
    pub new_target: Hash32, // commit we want remote ref to point to
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ObjectType {
    Blob,
    Tree,
    Commit,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushObject {
    pub object_type: ObjectType,
    pub hash: Hash32,
    pub data: Vec<u8>, // raw bytes from .helix/objects/*
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushAck {
    pub received_objects: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FetchRequest {
    pub repo: String,
    pub ref_name: String,                  // "refs/heads/main"
    pub last_known_remote: Option<Hash32>, // from refs/remotes/origin/main
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FetchObject {
    pub object_type: ObjectType,
    pub hash: Hash32,
    pub data: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FetchAck {
    pub sent_objects: u64,
    pub new_remote_head: Hash32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcError {
    pub code: u16,
    pub message: String,
}

#[derive(thiserror::Error, Debug)]
pub enum WireError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialize(#[from] Box<bincode::ErrorKind>),

    #[error("Unexpected EOF")]
    Eof,
}

/// length-prefixed bincode message:
/// [len: u32 LE][payload: len bytes]
/// can run this and read async too
pub fn write_message<W: Write>(mut w: W, msg: &RpcMessage) -> Result<(), WireError> {
    let payload = bincode::serialize(msg)?;
    let len = payload.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&payload)?;
    Ok(())
}

pub fn read_message<R: Read>(mut r: R) -> Result<RpcMessage, WireError> {
    let mut len_buf = [0u8; 4];
    if let Err(e) = r.read_exact(&mut len_buf) {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Err(WireError::Eof);
        }
        return Err(WireError::Io(e));
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    let msg: RpcMessage = bincode::deserialize(&payload)?;
    Ok(msg)
}
