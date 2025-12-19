use axum::{body::Body, response::Response};
use helix_protocol::{read_message, write_message, HelloAck, RpcError, RpcMessage};
use std::io::Cursor;

pub fn handle_handshake<T>(
    cursor: &mut Cursor<Vec<u8>>,
    out: &mut Vec<u8>,
    expect: fn(RpcMessage) -> Option<T>,
    expected_name: &'static str,
) -> Result<T, Response<Body>> {
    // Expect Hello and return HelloAck with the server version
    let ack_msg = match read_message(&mut *cursor) {
        Ok(RpcMessage::Hello(_h)) => RpcMessage::HelloAck(HelloAck {
            server_version: "helix-server".into(), // TODO: update this to be an actual server version
        }),
        Ok(other) => return Err(respond_err(400, format!("Expected Hello, got {:?}", other))),
        Err(e) => return Err(respond_err(400, format!("Failed to read Hello: {e}"))),
    };

    // write HelloAck back to the stream
    if let Err(e) = write_message(out, &ack_msg) {
        return Err(respond_err(500, format!("Failed to write HelloAck: {e}")));
    }

    // Expect the next message (PushRequest, PullRequest, etc.)
    let msg = match read_message(&mut *cursor) {
        Ok(m) => m,
        Err(e) => {
            return Err(respond_err(
                400,
                format!("Failed to read {expected_name}: {e}"),
            ))
        }
    };

    let msg_debug = format!("{:?}", msg);

    match expect(msg) {
        Some(v) => Ok(v),
        None => Err(respond_err(
            400,
            format!("Expected {expected_name}, got {msg_debug}"),
        )),
    }
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
