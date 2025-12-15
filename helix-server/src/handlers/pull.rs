use axum::{extract::State, response::IntoResponse};
use helix_protocol::{read_message, write_message, RpcError, RpcMessage};
use std::sync::Arc;

use super::push::AppState;

pub async fn pull_handler(
    State(_state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
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

    let response_msg = match msg {
        RpcMessage::Hello(h) => RpcMessage::HelloAck(helix_protocol::HelloAck {
            server_version: format!("helix-server-mvp (client = {})", h.client_version),
        }),
        _ => RpcMessage::Error(RpcError {
            code: 400,
            message: "Unexpected message type in pull_handler".into(),
        }),
    };

    let mut buf = Vec::new();
    write_message(&mut buf, &response_msg).unwrap();
    axum::response::Response::builder()
        .status(200)
        .body(axum::body::Body::from(buf))
        .unwrap()
}
