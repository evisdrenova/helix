use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum RpcMessage {
    // Handshake-ish (optional, but nice)
    Hello(Hello),
    HelloAck(HelloAck),

    // Push
    PushRequest(PushRequest),
    PushObject(PushObject),
    PushDone,
    PushAck(PushAck),

    // Fetch
    FetchRequest(FetchRequest),
    FetchObject(FetchObject),
    FetchDone,
    FetchAck(FetchAck),

    // Errors
    Error(RpcError),
}

#[derive(Serialize, Deserialize)]
pub struct Hello {
    pub client_version: String,
}

#[derive(Serialize, Deserialize)]
pub struct HelloAck {
    pub server_version: String,
}

#[derive(Serialize, Deserialize)]
pub struct PushRequest {
    pub repo: String,         // "org/name" or just repo name for MVP
    pub ref_name: String,     // "refs/heads/main"
    pub old_target: [u8; 32], // expected current value on server (or ZERO_HASH)
    pub new_target: [u8; 32], // commit we’re trying to push
}

#[derive(Serialize, Deserialize)]
pub enum ObjectType {
    Blob,
    Tree,
    Commit,
}

#[derive(Serialize, Deserialize)]
pub struct PushObject {
    pub object_type: ObjectType,
    pub hash: [u8; 32],
    pub data: Vec<u8>, // Helix-native bytes (already zstd-compressed)
}

#[derive(Serialize, Deserialize)]
pub struct PushAck {
    pub received_objects: u64,
}

#[derive(Serialize, Deserialize)]
pub struct FetchRequest {
    pub repo: String,
    pub ref_name: String,                    // which branch
    pub last_known_remote: Option<[u8; 32]>, // client’s remote-tracking commit
}

#[derive(Serialize, Deserialize)]
pub struct FetchObject {
    pub object_type: ObjectType,
    pub hash: [u8; 32],
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct FetchAck {
    pub sent_objects: u64,
    pub new_remote_head: [u8; 32],
}

#[derive(Serialize, Deserialize)]
pub struct RpcError {
    pub code: u16,
    pub message: String,
}
