use anyhow::{Context, Result};
use helix_protocol::hash::Hash;
use helix_protocol::message::{
    read_message, write_message, Hello, ObjectType, PullObject, PullRequest, RpcMessage,
};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

use crate::push::{read_remote_tracking, resolve_remote_and_ref, write_remote_tracking};

// update to use the pull_handshake fucntion similar to push_handshake
pub async fn pull(repo_path: &PathBuf, remote_name: &str, branch: &str) -> Result<()> {
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
        &RpcMessage::PullRequest(PullRequest {
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
            Ok(RpcMessage::PullObject(PullObject {
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
            Ok(RpcMessage::PullDone) => {
                // just continue; final ack follows
            }
            Ok(RpcMessage::PullAck(ack)) => {
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

fn write_local_object(
    repo_path: &Path,
    object_type: &ObjectType,
    hash: &Hash,
    data: &[u8],
) -> Result<()> {
    // Map protocol object type → local subdirectory name
    let subdir = match object_type {
        ObjectType::Blob => "blobs",
        ObjectType::Tree => "trees",
        ObjectType::Commit => "commits",
    };

    // .helix/objects/<subdir>/<hex_hash>
    let objects_dir = repo_path.join(".helix").join("objects").join(subdir);
    fs::create_dir_all(&objects_dir)
        .with_context(|| format!("Failed to create objects dir {}", objects_dir.display()))?;

    let filename = hex::encode(hash);
    let path = objects_dir.join(filename);

    fs::write(&path, data)
        .with_context(|| format!("Failed to write object to {}", path.display()))?;

    Ok(())
}
