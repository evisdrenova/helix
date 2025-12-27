use crate::handlers::utils::{handle_handshake, respond_err};
use axum::{extract::State, response::IntoResponse};
use helix_protocol::message::{write_message, ObjectType, PullAck, PullObject, RpcMessage};
use helix_server::app_state::AppState;
use helix_server::walk::collect_all_objects;
use std::io::Cursor;
use std::sync::Arc;

pub async fn pull_handler(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let mut cursor = Cursor::new(body.to_vec());
    let mut buf = Vec::<u8>::new();

    let pull_req = match handle_handshake(
        &mut cursor,
        &mut buf,
        |m| match m {
            RpcMessage::PullRequest(req) => Some(req),
            _ => None,
        },
        "PullRequest",
    ) {
        Ok(req) => req,
        Err(response) => return response,
    };

    let ref_name = pull_req.ref_name;

    let remote_head = match state.refs.get_ref(&ref_name) {
        Ok(Some(v)) => v,
        Ok(None) => {
            return respond_err(404, format!("Unknown ref {ref_name}"));
        }
        Err(e) => {
            return respond_err(500, format!("Failed to read ref: {e}"));
        }
    };

    // 3) Find objects to send
    let objects_to_send: Vec<(ObjectType, [u8; 32], Vec<u8>)> =
        collect_all_objects(&state.objects, &remote_head).unwrap_or_default();

    // 4) Build response body: FetchObject* + FetchDone + FetchAck
    for (ty, hash, data) in objects_to_send.iter() {
        let msg = RpcMessage::PullObject(PullObject {
            object_type: ty.clone(),
            hash: *hash,
            data: data.clone(),
        });
        if let Err(e) = write_message(&mut buf, &msg) {
            return respond_err(500, format!("Failed to encode FetchObject: {e}"));
        }
    }

    let done_msg = RpcMessage::PullDone;
    if let Err(e) = write_message(&mut buf, &done_msg) {
        return respond_err(500, format!("Failed to encode FetchDone: {e}"));
    }

    let ack_msg = RpcMessage::PullAck(PullAck {
        sent_objects: objects_to_send.len() as u64,
        new_remote_head: remote_head,
    });
    if let Err(e) = write_message(&mut buf, &ack_msg) {
        return respond_err(500, format!("Failed to encode FetchAck: {e}"));
    }

    axum::response::Response::builder()
        .status(200)
        .body(axum::body::Body::from(buf))
        .unwrap()
}
