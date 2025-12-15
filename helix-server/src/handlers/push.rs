use axum::{extract::State, response::IntoResponse};
use helix_protocol::{read_message, write_message, RpcError, RpcMessage};
use std::sync::Arc;

use crate::storage::storage::{FsObjectStore, FsRefStore};

#[derive(Clone)]
pub struct AppState {
    pub objects: FsObjectStore,
    pub refs: FsRefStore,
}

/// runs when user pushs to the server
pub async fn push_handler(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // We’ll fill this in milestone 2
    let mut cursor = std::io::Cursor::new(body.to_vec());

    let msg = match read_message(&mut cursor) {
        Ok(msg) => msg,
        Err(e) => {
            let err = RpcMessage::Error(RpcError {
                code: 400,
                message: format!("Failed to decode message: {e}"),
            });
            let mut buf = Vec::new();
            write_message(&mut buf, &err).unwrap();
            return axum::response::Response::builder()
                .status(400)
                .body(axum::body::Body::from(buf))
                .unwrap();
        }
    };

    // For now, just echo back HelloAck if we get Hello
    let response_msg = match msg {
        RpcMessage::Hello(h) => RpcMessage::HelloAck(helix_protocol::HelloAck {
            server_version: format!("helix-server-mvp (client = {})", h.client_version),
        }),
        _ => RpcMessage::Error(RpcError {
            code: 400,
            message: "Unexpected message type in push_handler".into(),
        }),
    };

    let mut buf = Vec::new();
    write_message(&mut buf, &response_msg).unwrap();
    axum::response::Response::builder()
        .status(200)
        .body(axum::body::Body::from(buf))
        .unwrap()
}
