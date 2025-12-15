// Push command - Push Helix commits to Git remote
// helix push <remote> <branch>
//
// Workflow:
// 1. Load current Helix HEAD
// 2. Convert Helix commits → Git commits (with caching)
// 3. Write Git objects to .git/objects/
// 4. Use gix to push to remote
// 5. Save push cache

use crate::helix_index::api::HelixIndexData;
use crate::helix_index::blob_storage::BlobStorage;
use crate::helix_index::commit::{CommitLoader, CommitStorage};
use crate::helix_index::hash::{self};
use crate::helix_index::tree::{EntryType, TreeStorage};
use anyhow::{Context, Result};
use helix_protocol::{
    read_message, write_message, Hash32, Hello, ObjectType, PushObject, PushRequest, RpcMessage,
};
use std::fs;
use std::io::Cursor;
use std::path::Path;

pub struct PushOptions {
    pub verbose: bool,
    pub dry_run: bool,
    pub force: bool,
}

impl Default for PushOptions {
    fn default() -> Self {
        Self {
            verbose: false,
            dry_run: false,
            force: false,
        }
    }
}

/// Push Helix commits to Git remote
///
/// Usage:
///   helix push origin main
///   helix push origin main --verbose
///   helix push origin feature-branch --force
///

/*
Helpers you’ll implement in your style:

resolve_remote_and_ref(repo_path, remote_name, branch) -> (url, ref_name)

Parse helix.toml with ini or toml (whatever you’re using).

read_local_ref – basically your existing read_head/branch helpers.

read_remote_tracking / write_remote_tracking – use .helix/refs/remotes/<remote>/<branch>.

compute_push_frontier – reuse your CommitStorage + tree traversal to get the set of hashes.

For MVP compute_push_frontier, you can ignore old_target and just “send everything reachable from new_target”, since remote might be empty. Later you can optimize. */
pub async fn push(
    repo_path: &Path,
    remote: &str,
    branch: &str,
    options: PushOptions,
) -> Result<()> {
    let repo_path = std::env::current_dir()?;
    let helix_dir = repo_path.join(".helix");
    if !helix_dir.exists() {
        anyhow::bail!("Not a Helix repo (no .helix directory)");
    }

    // 1. Resolve remote URL & ref name
    let (remote_url, ref_name) = resolve_remote_and_ref(&repo_path, remote_name, branch)?;

    // 2. Find local branch HEAD
    let new_target =
        read_local_ref(&repo_path, &ref_name).context("Failed to read local branch head")?;

    // 3. Determine old_target (from remote-tracking ref)
    let old_target = read_remote_tracking(&repo_path, remote_name, branch).unwrap_or([0u8; 32]); // ZERO_HASH

    // 4. Compute objects to send (commits + trees + blobs)
    let objects = compute_push_frontier(&repo_path, new_target, old_target)?;

    // 5. Build request body
    let mut buf = Vec::new();

    // Hello
    write_message(
        &mut buf,
        &RpcMessage::Hello(Hello {
            client_version: "helix-cli-mvp".into(),
        }),
    )?;

    // PushRequest
    write_message(
        &mut buf,
        &RpcMessage::PushRequest(PushRequest {
            repo: repo_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            ref_name: ref_name.clone(),
            old_target,
            new_target,
        }),
    )?;

    // PushObject*
    for (object_type, hash, data) in objects {
        write_message(
            &mut buf,
            &RpcMessage::PushObject(PushObject {
                object_type,
                hash,
                data,
            }),
        )?;
    }

    // PushDone
    write_message(&mut buf, &RpcMessage::PushDone)?;

    // 6. POST to /rpc/push
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{remote_url}/rpc/push"))
        .body(buf)
        .send()
        .await?;

    let status = resp.status();
    let bytes = resp.bytes().await?;
    let mut cursor = Cursor::new(bytes.to_vec());

    let msg = read_message(&mut cursor)?;

    match msg {
        RpcMessage::PushAck(ack) if status.is_success() => {
            println!(
                "Pushed {} objects to {remote_name}/{branch}",
                ack.received_objects
            );
            // Update local remote-tracking ref:
            write_remote_tracking(&repo_path, remote_name, branch, new_target)?;
            Ok(())
        }
        RpcMessage::Error(err) => {
            anyhow::bail!("Remote error {}: {}", err.code, err.message);
        }
        other => {
            anyhow::bail!(
                "Unexpected response from server: {:?} (status {status})",
                other
            );
        }
    }
}
