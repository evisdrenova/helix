use axum::{extract::State, response::IntoResponse};
use helix_server::walk::collect_all_objects;
use std::sync::Arc;
use helix_protocol::{
    read_message, write_message, FetchAck, FetchObject, FetchRequest, ObjectType, RpcError,
    RpcMessage,
};
use std::io::Cursor;
use super::push::AppState;

pub async fn pull_handler(
State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
let mut cursor = Cursor::new(body.to_vec());

    // 1) Expect Hello
    let _hello = match read_message(&mut cursor) {
        Ok(RpcMessage::Hello(h)) => h,
        Ok(other) => return respond_err(400, format!("Expected Hello, got {:?}", other)),
        Err(e) => return respond_err(400, format!("Error reading Hello: {e}")),
    };

    // 2) Expect FetchRequest
    let req = match read_message(&mut cursor) {
        Ok(RpcMessage::FetchRequest(req)) => req,
        Ok(other) => return respond_err(400, format!("Expected FetchRequest, got {:?}", other)),
        Err(e) => return respond_err(400, format!("Error reading FetchRequest: {e}")),
    };

    let ref_name = req.ref_name;
    let Some(remote_head) = match state.refs.get_ref(&ref_name) {
        Ok(v) => v,
        Err(e) => return respond_err(500, format!("Failed to read ref: {e}")),
    } else {
        return respond_err(404, format!("Unknown ref {ref_name}"));
    };

    // 3) Find objects to send
    // MVP: send "everything reachable from remote_head".
    // For now you can call into a small helper that:
    // - reads commit object
    // - follows parents
    // - collects commit hashes
    // - collects tree/blob hashes reachable from each commit
    let objects_to_send: Vec<(ObjectType, [u8; 32], Vec<u8>)> =
      collect_all_objects(&state.objects, &remote_head)
            .unwrap_or_default();

    // 4) Build response body: FetchObject* + FetchDone + FetchAck
    let mut buf = Vec::new();
    for (ty, hash, data) in objects_to_send.iter() {
        let msg = RpcMessage::FetchObject(FetchObject {
            object_type: ty.clone(),
            hash: *hash,
            data: data.clone(),
        });
        if let Err(e) = write_message(&mut buf, &msg) {
            return respond_err(500, format!("Failed to encode FetchObject: {e}"));
        }
    }

    let done_msg = RpcMessage::FetchDone;
    if let Err(e) = write_message(&mut buf, &done_msg) {
        return respond_err(500, format!("Failed to encode FetchDone: {e}"));
    }

    let ack_msg = RpcMessage::FetchAck(FetchAck {
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

fn respond_err(status: u16, msg: String) -> axum::response::Response {
    let err = RpcMessage::Error(RpcError { code: status, message: msg });
    let mut buf = Vec::new();
    write_message(&mut buf, &err).unwrap();
    axum::response::Response::builder()
        .status(status)
        .body(axum::body::Body::from(buf))
        .unwrap()
}