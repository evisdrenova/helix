use crate::handlers::utils::{handle_handshake, respond_err};
use crate::storage::storage::{FsObjectStore, FsRefStore};
use axum::{extract::State, response::IntoResponse};
use helix_protocol::{read_message, write_message, PushAck, PushObject, RpcMessage};
use std::io::Cursor;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub objects: FsObjectStore,
    pub refs: FsRefStore,
}

pub async fn push_handler(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let mut cursor = Cursor::new(body.to_vec());

    handle_handshake(&cursor);

    // write HelloAck to response buffer later; for now we don’t send partial responses,
    // so just ignore hello_msg and proceed.

    // 2) Expect PushRequest
    let push_req = match read_message(&mut cursor) {
        Ok(RpcMessage::PushRequest(req)) => req,
        Ok(other) => {
            return respond_err(400, format!("Expected PushRequest, got {:?}", other));
        }
        Err(e) => return respond_err(400, format!("Failed to read PushRequest: {e}")),
    };

    // MVP: we ignore push_req.repo and treat state.root as the repo.
    let ref_name = push_req.ref_name; // e.g. "refs/heads/main"

    // 3) Optional fast-forward check later; for now just overwrite
    // let current = state.refs.get_ref(&ref_name).unwrap_or(None);

    // 4) Receive PushObject* until PushDone
    let mut received_objects = 0u64;

    loop {
        match read_message(&mut cursor) {
            Ok(RpcMessage::PushObject(PushObject {
                object_type,
                hash,
                data,
            })) => {
                // verify object hash
                if blake3::hash(&data).as_bytes() != &hash {
                    return respond_err(400, "Hash mismatch for pushed object".into());
                }
                // Write if missing
                if !state.objects.has_object(&object_type, &hash) {
                    if let Err(e) = state.objects.write_object(&object_type, &hash, &data) {
                        return respond_err(500, format!("Failed to write object: {e}"));
                    }
                }
                received_objects += 1;
            }
            Ok(RpcMessage::PushDone) => break,
            Ok(other) => {
                return respond_err(400, format!("Unexpected message during push: {:?}", other));
            }
            Err(e) => {
                return respond_err(400, format!("Error reading message during push: {e}"));
            }
        }
    }

    // 5) Update ref
    if let Err(e) = state.refs.set_ref(&ref_name, push_req.new_target) {
        return respond_err(500, format!("Failed to update ref: {e}"));
    }

    let ack = RpcMessage::PushAck(PushAck { received_objects });

    let mut buf = Vec::new();
    if let Err(e) = write_message(&mut buf, &ack) {
        return respond_err(500, format!("Failed to encode PushAck: {e}"));
    }

    axum::response::Response::builder()
        .status(200)
        .body(axum::body::Body::from(buf))
        .unwrap()
}
