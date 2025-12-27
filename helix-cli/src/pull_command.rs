use anyhow::{bail, Context, Result};
use helix_protocol::hash::hash_to_hex;
use helix_protocol::message::{read_message, write_message, Hello, PullRequest, RpcMessage};
use helix_protocol::storage::FsObjectStore;
use std::{fs, io::Cursor, path::PathBuf};

use crate::handshake::pull_handshake;
use crate::push_command::{read_remote_tracking, resolve_remote_and_ref, write_remote_tracking};

pub struct PullOptions {
    pub verbose: bool,
    pub dry_run: bool,
}

impl Default for PullOptions {
    fn default() -> Self {
        Self {
            verbose: false,
            dry_run: false,
        }
    }
}

pub async fn pull(
    repo_path: &PathBuf,
    remote_name: &str,
    branch: &str,
    options: PullOptions,
) -> Result<()> {
    if !repo_path.join(".helix").exists() {
        bail!("Not a Helix repo (no .helix directory)");
    }

    let (remote_url, ref_name) = resolve_remote_and_ref(repo_path, remote_name, branch)?;

    // What we last knew about the remote (if anything)
    let last_known_remote = read_remote_tracking(repo_path, remote_name, branch).ok();

    if options.verbose {
        println!("Pulling {ref_name} from {remote_name} at {remote_url}");
        println!(
            "  last_known_remote = {}",
            last_known_remote
                .as_ref()
                .map(hash_to_hex)
                .unwrap_or_else(|| "<none>".to_string())
        );
    }

    // Handshake to see what the server has
    let remote_head = pull_handshake(
        &remote_url,
        &repo_path.file_name().unwrap_or_default().to_string_lossy(),
        &ref_name,
        last_known_remote,
    )
    .await?;

    // Check if we're already up to date
    if let Some(remote_hash) = remote_head {
        if last_known_remote == Some(remote_hash) {
            println!("Already up to date.");
            return Ok(());
        }
    } else {
        println!("Remote branch {} does not exist.", branch);
        return Ok(());
    }

    if options.dry_run {
        println!("(dry run) Would pull from {} to {}", remote_name, branch);
        return Ok(());
    }

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
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            ref_name: ref_name.clone(),
            last_known_remote: last_known_remote,
        }),
    )?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{remote_url}/rpc/pull"))
        .body(buf)
        .send()
        .await
        .with_context(|| "Connection to server lost during data transfer.")?;

    let status = resp.status();
    let bytes = resp.bytes().await?;

    if !status.is_success() {
        bail!("Server returned error status: {}", status);
    }

    let mut cursor = Cursor::new(bytes.to_vec());

    // Process the response: HelloAck, then FetchObjects, then FetchDone, then FetchAck
    // Skip HelloAck
    match read_message(&mut cursor)? {
        RpcMessage::PullAck(_) => {
            if options.verbose {
                println!("Received HelloAck from server");
            }
        }
        RpcMessage::Error(err) => {
            bail!("Remote error {}: {}", err.code, err.message);
        }
        other => {
            bail!("Expected HelloAck, got {:?}", other);
        }
    }

    let store = FsObjectStore::new(repo_path);
    let mut received_objects = 0u64;

    loop {
        match read_message(&mut cursor) {
            Ok(RpcMessage::PullObject(obj)) => {
                // Verify hash
                let computed_hash = blake3::hash(&obj.data);
                if computed_hash.as_bytes() != &obj.hash {
                    bail!(
                        "Hash mismatch for received object: expected {}, got {}",
                        hash_to_hex(&obj.hash),
                        hex::encode(computed_hash.as_bytes())
                    );
                }

                // Write object to local store
                if !store.has_object(&obj.object_type, &obj.hash) {
                    store
                        .write_object_with_hash(&obj.object_type, &obj.hash, &obj.data)
                        .with_context(|| {
                            format!(
                                "Failed to write {:?} object {}",
                                obj.object_type,
                                hash_to_hex(&obj.hash)
                            )
                        })?;
                }

                received_objects += 1;

                if options.verbose {
                    println!(
                        "  Received {:?}: {}",
                        obj.object_type,
                        &hash_to_hex(&obj.hash)[..8]
                    );
                }
            }
            Ok(RpcMessage::PullDone) => {
                if options.verbose {
                    println!("Received FetchDone");
                }
                break;
            }
            Ok(RpcMessage::Error(err)) => {
                bail!("Remote error during fetch: {} - {}", err.code, err.message);
            }
            Ok(other) => {
                bail!("Unexpected message during fetch: {:?}", other);
            }
            Err(e) => {
                bail!("Error reading message during fetch: {}", e);
            }
        }
    }

    // Read FetchAck
    let new_remote_head = match read_message(&mut cursor) {
        Ok(RpcMessage::PullAck(ack)) => {
            if options.verbose {
                println!(
                    "FetchAck: {} objects sent, new head: {}",
                    ack.sent_objects,
                    hash_to_hex(&ack.new_remote_head)
                );
            }
            ack.new_remote_head
        }
        Ok(RpcMessage::Error(err)) => {
            bail!("Remote error in FetchAck: {} - {}", err.code, err.message);
        }
        Ok(other) => {
            bail!("Expected FetchAck, got {:?}", other);
        }
        Err(e) => {
            bail!("Error reading FetchAck: {}", e);
        }
    };

    // Update remote tracking ref
    write_remote_tracking(repo_path, remote_name, branch, new_remote_head)?;

    // Update local branch ref to point to the new head
    let local_ref_path = repo_path.join(".helix").join(&ref_name);
    if let Some(parent) = local_ref_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&local_ref_path, hash_to_hex(&new_remote_head) + "\n")?;

    println!(
        "Pulled {} objects from {}/{}",
        received_objects, remote_name, branch
    );
    println!(
        "Updated {} to {}",
        ref_name,
        &hash_to_hex(&new_remote_head)[..8]
    );

    Ok(())
}
