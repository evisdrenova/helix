use std::io::Cursor;

use anyhow::{bail, Context, Result};
use helix_protocol::hash::Hash;
use helix_protocol::message::{read_message, write_message, Hello, PushRequest, RpcMessage};

pub async fn push_handshake(
    remote_url: &str,
    repo_name: &str,
    ref_name: &str,
    new_target: Hash,
    old_target: Option<Hash>,
) -> Result<Option<Hash>> {
    let mut buf: Vec<u8> = Vec::new();

    write_message(
        &mut buf,
        &RpcMessage::Hello(Hello {
            client_version: "helix-cli".into(),
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

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let resp = client
        .post(format!("{remote_url}/rpc/handshake"))
        .body(buf)
        .send()
        .await
        .with_context(|| {
            format!("Remote server at {remote_url} is unreachable. Is the Helix server running?")
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let error_body = resp.text().await.unwrap_or_default();
        bail!("Remote returned error {}: {}", status, error_body);
    }

    let bytes = resp.bytes().await?;
    let mut cursor = Cursor::new(bytes.to_vec());

    match read_message(&mut cursor)? {
        RpcMessage::PushResponse(r) => {
            // TODO: update with actual server response with what it has already, so we can calc diff between new and old
            println!("Connected to the server!");
            let head_display = match r.remote_head {
                Some(hash) => hex::encode(hash),
                None => "0".repeat(64).to_string(),
            };
            println!("Server is currently at: {}", head_display);
            Ok(r.remote_head)
        }
        RpcMessage::Error(err) => {
            bail!(
                "Remote error {} during handshake: {}",
                err.code,
                err.message
            );
        }
        _ => bail!("Unexpected response during handshake"),
    }
}
