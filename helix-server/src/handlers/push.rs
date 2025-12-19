use crate::handlers::utils::{handle_handshake, respond_err};
use axum::{extract::State, response::IntoResponse};
use helix_protocol::{read_message, write_message, PushAck, PushObject, RpcMessage};
use helix_server::app_state::AppState;
use std::io::Cursor;
use std::sync::Arc;

pub async fn push_handler(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    println!(">>> /rpc/push called, body size = {}", body.len());

    let mut cursor = Cursor::new(body.to_vec());

    // Collect all outbound RPC messages in one HTTP response body
    let mut out = Vec::<u8>::new();

    let push_req = match handle_handshake(
        &mut cursor,
        &mut out,
        |m| match m {
            RpcMessage::PushRequest(req) => Some(req),
            _ => None,
        },
        "PushRequest",
    ) {
        Ok(req) => req,
        Err(response) => return response,
    };

    // Receive PushObject* until PushDone
    let mut received_objects = 0u64;

    loop {
        match read_message(&mut cursor) {
            Ok(RpcMessage::PushObject(PushObject {
                object_type,
                hash,
                data,
            })) => {
                println!("reading RPC message from client");

                // check hash integrity
                if blake3::hash(&data).as_bytes() != &hash {
                    return respond_err(400, "Hash mismatch for pushed object".into());
                }

                // object store doesn't have hash and data, then write it to fsobjectstore
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
            Err(e) => return respond_err(400, format!("Error reading message during push: {e}")),
        }
    }

    // Update ref to point to latest target
    if let Err(e) = state.refs.set_ref(&push_req.ref_name, push_req.new_target) {
        return respond_err(500, format!("Failed to update ref: {e}"));
    }

    // Write PushAck to `out`
    let ack = RpcMessage::PushAck(PushAck { received_objects });
    if let Err(e) = write_message(&mut out, &ack) {
        return respond_err(500, format!("Failed to encode PushAck: {e}"));
    }

    axum::response::Response::builder()
        .status(200)
        .header(axum::http::header::CONTENT_TYPE, "application/octet-stream")
        .body(axum::body::Body::from(out))
        .unwrap()
}
