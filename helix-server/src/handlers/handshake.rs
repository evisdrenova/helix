/// Handles the handshake between the client and the server
/// we don't directly acknowledge the Hello request from the client
/// by sending back a Push/Pull Response, we're acknowledging that everything is fine using only one request
use crate::handlers::utils::respond_err;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use helix_protocol::message::{
    read_message, write_message, PullRequest, PullResponse, PushResponse, RpcMessage,
};
use helix_server::app_state::AppState;
use std::io::Cursor;
use std::sync::Arc;

pub async fn handshake_handler(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, Response> {
    let mut cursor = Cursor::new(body.to_vec());

    // read hello
    match read_message(&mut cursor) {
        Ok(RpcMessage::Hello(_)) => {} // handle version matches and stuff here
        _ => return Err(respond_err(400, "Missing Hello".into())),
    };

    // read the next message
    let msg = read_message(&mut cursor)
        .map_err(|e| respond_err(400, format!("Failed to read next message after Hello: {e}")))?;

    match msg {
        RpcMessage::PushRequest(req) => {
            let mut out = Vec::new();

            let remote_head = state
                .refs
                .get_ref(&req.ref_name)
                .map_err(|e| respond_err(500, format!("get_ref failed: {e}")))?;

            let reply = RpcMessage::PushResponse(PushResponse { remote_head });

            write_message(&mut out, &reply)
                .map_err(|e| respond_err(500, format!("Failed to write PushResponse: {e}")))?;

            Ok(out)
        }
        RpcMessage::PullRequest(PullRequest {
            ref_name,
            last_known_remote: _,
            ..
        }) => {
            let mut out = Vec::new();

            // For now we just return the current remote head.
            // Later you can use last_known_remote to decide if the client is already up-to-date,
            // or to send "need these objects" hints.
            let remote_head = state
                .refs
                .get_ref(&ref_name)
                .map_err(|e| respond_err(500, format!("get_ref failed: {e}")))?;

            let reply = RpcMessage::PullResponse(PullResponse { remote_head });

            write_message(&mut out, &reply)
                .map_err(|e| respond_err(500, format!("Failed to write PullResponse: {e}")))?;

            Ok(out)
        }
        other => Err(respond_err(400, format!("Unexpected message: {:?}", other))),
    }
}
