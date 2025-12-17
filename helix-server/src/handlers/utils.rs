use std::io::Cursor;

use anyhow::Result;
use helix_protocol::{read_message, write_message, HelloAck, RpcError, RpcMessage};

pub fn handle_handshake(cursor: &Cursor<Vec<u8>>) -> axum::response::Response {
    // 1) Expect Hello
    let hello_msg = match read_message(&mut cursor) {
        Ok(RpcMessage::Hello(_h)) => RpcMessage::HelloAck(HelloAck {
            server_version: "helix-server-mvp".into(),
        }),
        Ok(other) => {
            return respond_err(400, format!("Expected Hello, got {:?}", other));
        }
        Err(e) => return respond_err(400, format!("Failed to read Hello: {e}")),
    };
}

pub fn respond_err(status: u16, msg: String) -> axum::response::Response {
    let err = RpcMessage::Error(RpcError {
        code: status,
        message: msg,
    });
    let mut buf = Vec::new();
    write_message(&mut buf, &err).unwrap();
    axum::response::Response::builder()
        .status(status)
        .body(axum::body::Body::from(buf))
        .unwrap()
}
