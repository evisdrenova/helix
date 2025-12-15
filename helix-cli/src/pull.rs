use anyhow::{Context, Result};
use helix_protocol::{read_message, write_message, FetchObject, FetchRequest, Hello, RpcMessage};
use std::io::Cursor;

pub async fn pull(remote_name: &str, branch: &str) -> Result<()> {
    let repo_path = std::env::current_dir()?;
    let (remote_url, ref_name) = resolve_remote_and_ref(&repo_path, remote_name, branch)?;

    let last_known = read_remote_tracking(&repo_path, remote_name, branch).ok();

    // Build request
    let mut buf = Vec::new();
    write_message(
        &mut buf,
        &RpcMessage::Hello(Hello {
            client_version: "helix-cli-mvp".into(),
        }),
    )?;

    write_message(
        &mut buf,
        &RpcMessage::FetchRequest(FetchRequest {
            repo: repo_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            ref_name: ref_name.clone(),
            last_known_remote: last_known,
        }),
    )?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{remote_url}/rpc/fetch"))
        .body(buf)
        .send()
        .await?;

    let status = resp.status();
    let bytes = resp.bytes().await?;
    let mut cursor = Cursor::new(bytes.to_vec());

    let mut new_remote_head = None;
    let mut received_objects = 0u64;

    loop {
        match read_message(&mut cursor) {
            Ok(RpcMessage::FetchObject(FetchObject {
                object_type,
                hash,
                data,
            })) => {
                // verify hash & write to local object store
                if blake3::hash(&data).as_bytes() != &hash {
                    anyhow::bail!("Hash mismatch for fetched object");
                }
                write_local_object(&repo_path, &object_type, &hash, &data)?;
                received_objects += 1;
            }
            Ok(RpcMessage::FetchDone) => {
                // just continue; final ack follows
            }
            Ok(RpcMessage::FetchAck(ack)) => {
                new_remote_head = Some(ack.new_remote_head);
                break;
            }
            Ok(RpcMessage::Error(err)) => {
                anyhow::bail!("Remote error {}: {}", err.code, err.message);
            }
            Ok(other) => {
                anyhow::bail!("Unexpected fetch response: {:?}", other);
            }
            Err(e) => {
                anyhow::bail!("Error decoding fetch response: {e}");
            }
        }
    }

    if !status.is_success() {
        anyhow::bail!("HTTP error from remote: {status}");
    }

    if let Some(head) = new_remote_head {
        // update remote-tracking ref
        write_remote_tracking(&repo_path, remote_name, branch, head)?;
        println!(
            "Fetched {} objects; updated remotes/{}/{} -> {}",
            received_objects,
            remote_name,
            branch,
            hex::encode(head)
        );
    }

    Ok(())
}
