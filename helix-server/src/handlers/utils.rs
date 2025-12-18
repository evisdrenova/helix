use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
};
use helix_protocol::{read_message, write_message, HelloAck, RpcError, RpcMessage};
use std::io::Cursor;

pub fn handle_handshake(cursor: &mut Cursor<Vec<u8>>) -> Response {
    // ecxpect hello
    let ack_msg = match read_message(cursor) {
        Ok(RpcMessage::Hello(_h)) => RpcMessage::HelloAck(HelloAck {
            server_version: "helix-server".into(),
        }),
        Ok(other) => return respond_err(400, format!("Expected Hello, got {:?}", other)),
        Err(e) => return respond_err(400, format!("Failed to read Hello: {e}")),
    };

    // write helloack out to bytes
    let mut out = Vec::new();
    if let Err(e) = write_message(&mut out, &ack_msg) {
        return respond_err(500, format!("Failed to write HelloAck: {e}"));
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(out))
        .unwrap()
}

pub fn respond_err(status: u16, msg: String) -> Response {
    let err = RpcMessage::Error(RpcError {
        code: status,
        message: msg,
    });
    let mut buf = Vec::new();
    write_message(&mut buf, &err).unwrap();
    Response::builder()
        .status(status)
        .body(axum::body::Body::from(buf))
        .unwrap()
}
