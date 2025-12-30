use crate::handlers::utils::{handle_handshake, respond_err};
use axum::{extract::State, response::IntoResponse};
use helix_protocol::commit::{collect_objects_from_commits, walk_commits_between};
use helix_protocol::message::{write_message, PullAck, PullObject, RpcMessage};
use helix_server::app_state::AppState;
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

    let ref_name = &pull_req.ref_name;

    // 1. Get remote head
    let remote_head = match state.refs.get_ref(ref_name) {
        Ok(Some(v)) => v,
        Ok(None) => {
            // Ref doesn't exist - return PullAck with ref_not_found flag
            let ack = RpcMessage::PullAck(PullAck {
                sent_objects: 0,
                new_remote_head: [0u8; 32],
                up_to_date: false,
                ref_not_found: true,
            });
            if let Err(e) = write_message(&mut buf, &ack) {
                return respond_err(500, format!("Failed to encode PullAck: {e}"));
            }
            return axum::response::Response::builder()
                .status(200)
                .header("Content-Type", "application/octet-stream")
                .body(axum::body::Body::from(buf))
                .unwrap();
        }
        Err(e) => {
            return respond_err(500, format!("Failed to read ref: {e}"));
        }
    };

    // 2. Check if already up to date
    if pull_req.last_known_remote == Some(remote_head) {
        let ack = RpcMessage::PullAck(PullAck {
            sent_objects: 0,
            new_remote_head: remote_head,
            up_to_date: true,
            ref_not_found: false,
        });
        if let Err(e) = write_message(&mut buf, &ack) {
            return respond_err(500, format!("Failed to encode PullAck: {e}"));
        }
        return axum::response::Response::builder()
            .status(200)
            .header("Content-Type", "application/octet-stream")
            .body(axum::body::Body::from(buf))
            .unwrap();
    }

    // 3. Walk commit graph to find missing commits
    let missing_commits =
        match walk_commits_between(&state.objects, remote_head, pull_req.last_known_remote) {
            Ok(commits) => commits,
            Err(e) => {
                return respond_err(500, format!("Failed to walk commits: {e}"));
            }
        };

    // 4. Collect all objects (commits + trees + blobs)
    let objects_to_send = match collect_objects_from_commits(&state.objects, &missing_commits) {
        Ok(objects) => objects,
        Err(e) => {
            return respond_err(500, format!("Failed to collect objects: {e}"));
        }
    };

    // 5. Stream objects
    for (ty, hash, data) in &objects_to_send {
        let msg = RpcMessage::PullObject(PullObject {
            object_type: ty.clone(),
            hash: *hash,
            data: data.clone(),
        });
        if let Err(e) = write_message(&mut buf, &msg) {
            return respond_err(500, format!("Failed to encode PullObject: {e}"));
        }
    }

    // 6. Send PullDone
    if let Err(e) = write_message(&mut buf, &RpcMessage::PullDone) {
        return respond_err(500, format!("Failed to encode PullDone: {e}"));
    }

    // 7. Send PullAck
    let ack = RpcMessage::PullAck(PullAck {
        sent_objects: objects_to_send.len() as u64,
        new_remote_head: remote_head,
        up_to_date: false,
        ref_not_found: false,
    });
    if let Err(e) = write_message(&mut buf, &ack) {
        return respond_err(500, format!("Failed to encode PullAck: {e}"));
    }

    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/octet-stream")
        .body(axum::body::Body::from(buf))
        .unwrap()
}
