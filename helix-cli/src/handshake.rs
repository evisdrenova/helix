use std::io::Cursor;

use anyhow::{bail, Result};
use helix_protocol::{read_message, write_message, Hash32, Hello, PushRequest, RpcMessage};

// handshake between the CLI and Server
pub async fn push_handshake(
    remote_url: &str,
    repo_name: &str,
    ref_name: &str,
    new_target: Hash32,
    old_target: Option<Hash32>,
) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();

    write_message(
        &mut buf,
        &RpcMessage::Hello(Hello {
            client_version: "helix-cli-mvp".into(),
        }),
    )?;

    write_message(
        &mut buf,
        &RpcMessage::PushRequest(PushRequest {
            repo: repo_name.to_string(),
            ref_name: ref_name.to_string(),
            old_target: old_target.unwrap_or([0u8; 32]),
            new_target,
        }),
    )?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{remote_url}/rpc/handshake"))
        .body(buf)
        .send()
        .await;

    println!("1");

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            bail!("Failed to reach Helix remote at {remote_url}: {e}");
        }
    };

    let status = resp.status();
    let bytes = resp.bytes().await?;
    let mut cursor = Cursor::new(bytes.to_vec());

    let first_msg = read_message(&mut cursor)?;
    match first_msg {
        RpcMessage::HelloAck(_) => println!("Handshake: HelloAck received"),
        RpcMessage::Error(e) => bail!("Server error: {}", e.message),
        _ => bail!("Expected HelloAck, got {:?}", first_msg),
    }

    println!("2 {:?}", status);

    let second_message = read_message(&mut cursor)?;

    match second_message {
        RpcMessage::PushResponse(r) if status.is_success() => {
            // TODO: update with actual server response with what it has already, so we can calc diff between new and old
            println!("3 {:?}", r.remote_head);
            Ok(())
        }
        RpcMessage::Error(err) => {
            bail!(
                "Remote error {} during handshake: {}",
                err.code,
                err.message
            );
        }
        other => {
            bail!(
                "Unexpected response from server during handshake: {:?} (status {status})",
                other
            );
        }
    }
}
