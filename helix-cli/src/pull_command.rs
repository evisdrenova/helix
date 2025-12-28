use anyhow::{bail, Context, Result};
use helix_protocol::hash::hash_to_hex;
use helix_protocol::message::{read_message, write_message, Hello, PullRequest, RpcMessage};
use helix_protocol::storage::FsObjectStore;
use rayon::prelude::*;
use std::{fs, io::Cursor, path::Path};

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
    repo_path: &Path,
    remote_name: &str,
    branch: &str,
    options: PullOptions,
) -> Result<()> {
    if !repo_path.join(".helix").exists() {
        bail!("Not a Helix repo (no .helix directory)");
    }

    let (remote_url, ref_name) = resolve_remote_and_ref(repo_path, remote_name, branch)?;
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

    if options.dry_run {
        println!("(dry run) Would pull from {}/{}", remote_name, branch);
        return Ok(());
    }

    // Build pull request
    let mut buf = Vec::new();

    write_message(
        &mut buf,
        &RpcMessage::Hello(Hello {
            client_version: "helix-cli".into(),
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
            last_known_remote,
        }),
    )?;

    // Send request
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{remote_url}/rpc/pull"))
        .body(buf)
        .send()
        .await
        .with_context(|| {
            format!("Remote server at {remote_url} is unreachable. Is the Helix server running?")
        })?;

    let status = resp.status();
    if !status.is_success() {
        bail!("Server returned error: {}", status);
    }

    let bytes = resp.bytes().await?;
    let mut cursor = Cursor::new(bytes.to_vec());

    // Collect objects for parallel writes
    let store = FsObjectStore::new(repo_path);
    let mut objects_to_write = Vec::new();

    loop {
        match read_message(&mut cursor) {
            Ok(RpcMessage::PullObject(obj)) => {
                // Verify hash
                let computed = blake3::hash(&obj.data);
                if computed.as_bytes() != &obj.hash {
                    bail!(
                        "Hash mismatch: expected {}, got {}",
                        hash_to_hex(&obj.hash),
                        hex::encode(computed.as_bytes())
                    );
                }

                objects_to_write.push(obj);

                if options.verbose && objects_to_write.len() % 100 == 0 {
                    println!("  Received {} objects...", objects_to_write.len());
                }
            }
            Ok(RpcMessage::PullDone) => {
                if options.verbose {
                    println!("Received PullDone");
                }
                break;
            }
            Ok(RpcMessage::PullAck(ack)) => {
                // Server sends PullAck directly for up-to-date or empty cases
                if ack.up_to_date {
                    println!("Already up to date.");
                    return Ok(());
                }
                // If sent_objects is 0 but not up_to_date, branch might not exist
                if ack.sent_objects == 0 {
                    println!("Remote branch {} has no commits.", branch);
                    return Ok(());
                }
                bail!("Unexpected PullAck before PullDone");
            }
            Ok(RpcMessage::Error(err)) => {
                bail!("Server error: {} - {}", err.code, err.message);
            }
            Ok(other) => {
                bail!("Unexpected message: {:?}", other);
            }
            Err(e) => {
                bail!("Error reading message: {}", e);
            }
        }
    }

    // Write objects in parallel
    let object_count = objects_to_write.len();
    if options.verbose {
        println!("Writing {} objects to store...", object_count);
    }

    objects_to_write
        .par_iter()
        .try_for_each(|obj| -> Result<()> {
            if !store.has_object(&obj.object_type, &obj.hash) {
                store.write_object_with_hash(&obj.object_type, &obj.hash, &obj.data)?;
            }
            Ok(())
        })?;

    // Read final PullAck
    let new_remote_head = match read_message(&mut cursor) {
        Ok(RpcMessage::PullAck(ack)) => {
            if options.verbose {
                println!(
                    "PullAck: {} objects, new head: {}",
                    ack.sent_objects,
                    hash_to_hex(&ack.new_remote_head)
                );
            }
            ack.new_remote_head
        }
        Ok(RpcMessage::Error(err)) => {
            bail!("Server error: {} - {}", err.code, err.message);
        }
        Ok(other) => {
            bail!("Expected PullAck, got {:?}", other);
        }
        Err(e) => {
            bail!("Error reading PullAck: {}", e);
        }
    };

    // Update refs
    write_remote_tracking(repo_path, remote_name, branch, new_remote_head)?;

    let local_ref_path = repo_path.join(".helix").join(&ref_name);
    if let Some(parent) = local_ref_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&local_ref_path, hash_to_hex(&new_remote_head) + "\n")?;

    println!(
        "Pulled {} objects from {}/{}",
        object_count, remote_name, branch
    );
    println!(
        "Updated {} -> {}",
        ref_name,
        &hash_to_hex(&new_remote_head)[..8]
    );

    Ok(())
}
