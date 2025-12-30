mod handlers;

use axum::{routing::post, Router};
use helix_protocol::storage::{FsObjectStore, FsRefStore};
use helix_server::app_state::AppState;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::handlers::{handshake::handshake_handler, pull::pull_handler, push::push_handler};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // TODO: single root dir for one repo, update to support multiple
    let repo_root = std::env::var("HELIX_REPO_ROOT").unwrap_or_else(|_| ".".to_string());

    let objects = FsObjectStore::new(&repo_root);
    let refs = FsRefStore::new(&repo_root);

    let state = Arc::new(AppState { objects, refs });
    // TODO: later let's move to a real streaming reader inside the handlers like from a TCP socket or chunked body since right nwo the entire HTTP body is buffered - would likely be more efficient
    let app = Router::new()
        .route("/rpc/handshake", post(handshake_handler))
        .route("/rpc/push", post(push_handler))
        .route("/rpc/pull", post(pull_handler))
        .with_state(state);

    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    println!("helix-server listening on {}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}
