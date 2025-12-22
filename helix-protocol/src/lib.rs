/*
┌──────────────┐          HTTP POST (binary RPC stream)          ┌──────────────┐
│  helix CLI   │  -------------------------------------------->  │ helix-server │
│  (push cmd)  │                                                  │  (/rpc/push) │
└──────────────┘  <--------------------------------------------  └──────────────┘
          build + send messages                         read + handle messages
             (Hello, PushRequest, PushObject*, PushDone)  (… then PushAck back)


*/

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

/// Output of blake3 hashing
pub type Hash32 = [u8; 32];

#[derive(Debug, Serialize, Deserialize)]
pub enum RpcMessage {
    Hello(Hello),

    PushRequest(PushRequest),
    PushResponse(PushResponse),
    PushObject(PushObject),
    PushDone,
    PushAck(PushAck),

    PullRequest(PullRequest),
    PullObject(PullObject),
    PullDone,
    PullAck(PullAck),

    Error(RpcError),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub client_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushRequest {
    pub repo: String,       // just repo name or path
    pub ref_name: String,   // which pointer -> "refs/heads/main"
    pub old_target: Hash32, // expected current value of the ref on the server; ZERO_HASH for "no remote yet", if the server's refs/head/main points to commit A, then old_target = A, branch is just a pointer to a commit
    pub new_target: Hash32, // commit hash the ref should point to after the push
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushResponse {
    pub remote_head: Option<Hash32>,
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
pub struct PullRequest {
    pub repo: String,
    pub ref_name: String,                  // "refs/heads/main"
    pub last_known_remote: Option<Hash32>, // from refs/remotes/origin/main
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullObject {
    pub object_type: ObjectType,
    pub hash: Hash32,
    pub data: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullAck {
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
