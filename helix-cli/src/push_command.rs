use anyhow::{bail, Context, Result};
use helix_protocol::commit::{parse_commit_for_walk, CommitData};
use helix_protocol::hash::{hash_to_hex, hex_to_hash, Hash};
use helix_protocol::message::{
    read_message, write_message, Hello, ObjectType, PushObject, PushRequest, RpcMessage,
};
use helix_protocol::storage::FsObjectStore;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::Cursor;
use std::path::Path;

use crate::handshake::push_handshake;
use crate::init_command::HelixConfig;

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

pub async fn push(
    repo_path: &Path,
    remote_name: &str,
    branch: &str,
    options: PushOptions,
) -> Result<()> {
    if !repo_path.join(".helix").exists() {
        bail!("Not a Helix repo (no .helix directory)");
    }

    let (remote_url, ref_name) = resolve_remote_and_ref(&repo_path, remote_name, branch)?;

    let new_target =
        read_local_ref(&repo_path, &ref_name).context("Failed to read local branch head")?;

    let old_target = read_remote_tracking(repo_path, remote_name, branch).ok();

    if options.verbose {
        println!("Pushing {ref_name} to {remote_name} at {remote_url}");
        println!(
            "  old_target = {}",
            old_target
                .as_ref()
                .map(hash_to_hex)
                .unwrap_or_else(|| "<none>".to_string())
        );
        println!("  new_target = {}", hash_to_hex(&new_target));
    }

    let server_head = push_handshake(
        &remote_url,
        &repo_path.file_name().unwrap_or_default().to_string_lossy(),
        &ref_name,
        new_target,
        old_target,
    )
    .await?;

    if options.dry_run {
        println!("(dry run) Would push from {} to {}", remote_name, branch);
        return Ok(());
    }

    // Compute objects to send (MVP: send everything we have)
    // TODO: update this only send teh difference between the remote and local by walking from new_target back to old_target
    // TODO: as we create the objects, we should write them to the buffer at the same time instead of doing it in sequence
    let store = FsObjectStore::new(repo_path);
    let objects = compute_objects_to_push(&store, new_target, server_head)?;

    if objects.is_empty() {
        println!("Everything up to date.");
        return Ok(());
    }

    if options.verbose {
        println!("Sending {} objects...", objects.len());
    }

    let mut buf = Vec::new();

    write_message(
        &mut buf,
        &RpcMessage::Hello(Hello {
            client_version: "helix-cli".into(),
        }),
    )?;

    write_message(
        &mut buf,
        &RpcMessage::PushRequest(PushRequest {
            repo: repo_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            ref_name: ref_name.clone(),
            old_target: old_target.unwrap_or([0u8; 32]),
            new_target,
        }),
    )?;

    // write objects to the buf to send to the server
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

    write_message(&mut buf, &RpcMessage::PushDone)?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{remote_url}/rpc/push"))
        .body(buf)
        .send()
        .await
        .with_context(|| "Connection to server lost during data transfer.")?;

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
            write_remote_tracking(repo_path, remote_name, branch, new_target)?;
            Ok(())
        }
        RpcMessage::Error(err) => {
            bail!("Remote error {}: {}", err.code, err.message);
        }
        other => {
            bail!(
                "Unexpected response from server: {:?} (status {status})",
                other
            );
        }
    }
}

/// Compute objects to push by walking from new_target back to server_head.
/// Only sends commits, trees, and blobs that the server doesn't have.
fn compute_objects_to_push(
    store: &FsObjectStore,
    new_target: Hash,
    server_head: Option<Hash>,
) -> Result<Vec<(ObjectType, Hash, Vec<u8>)>> {
    // Walk commits from new_target back to server_head
    let missing_commits = walk_commits_between(store, new_target, server_head)?;

    if missing_commits.is_empty() {
        return Ok(vec![]);
    }

    // Collect all objects from those commits
    collect_objects_from_commits(store, &missing_commits)
}

/// Walk from `from` (new_target) backwards until we hit `to` (server_head) or run out of parents.
fn walk_commits_between(
    store: &FsObjectStore,
    from: Hash,
    to: Option<Hash>,
) -> Result<Vec<CommitData>> {
    let mut result = Vec::new();
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();

    queue.push_back(from);

    while let Some(hash) = queue.pop_front() {
        // Stop if we've reached what server already has
        if to == Some(hash) {
            continue;
        }

        if !seen.insert(hash) {
            continue;
        }

        // Read commit bytes
        let commit_bytes = match store.read_object(&ObjectType::Commit, &hash) {
            Ok(bytes) => bytes,
            Err(_) => continue, // Skip if we can't read it
        };

        let (tree_hash, parents) = parse_commit_for_walk(&commit_bytes)?;

        // Queue parents
        for parent in &parents {
            queue.push_back(*parent);
        }

        result.push(CommitData {
            hash,
            tree_hash,
            bytes: commit_bytes,
        });
    }

    Ok(result)
}

/// Collect all objects needed: commits, trees, and blobs
fn collect_objects_from_commits(
    store: &FsObjectStore,
    commits: &[CommitData],
) -> Result<Vec<(ObjectType, Hash, Vec<u8>)>> {
    let mut objects = Vec::new();
    let mut seen_trees = HashSet::new();
    let mut seen_blobs = HashSet::new();

    // Add commits first
    for commit in commits {
        objects.push((ObjectType::Commit, commit.hash, commit.bytes.clone()));
    }

    // Collect trees and blobs from each commit's tree
    for commit in commits {
        collect_tree_recursive(
            store,
            commit.tree_hash,
            &mut seen_trees,
            &mut seen_blobs,
            &mut objects,
        )?;
    }

    Ok(objects)
}

/// Recursively collect a tree and all its blobs/subtrees.
fn collect_tree_recursive(
    store: &FsObjectStore,
    tree_hash: Hash,
    seen_trees: &mut HashSet<Hash>,
    seen_blobs: &mut HashSet<Hash>,
    objects: &mut Vec<(ObjectType, Hash, Vec<u8>)>,
) -> Result<()> {
    if !seen_trees.insert(tree_hash) {
        return Ok(()); // Already processed
    }

    let tree_bytes = store.read_object(&ObjectType::Tree, &tree_hash)?;
    objects.push((ObjectType::Tree, tree_hash, tree_bytes.clone()));

    // Parse tree entries
    let entries = parse_tree_entries(&tree_bytes)?;

    for (entry_kind, hash) in entries {
        match entry_kind {
            EntryKind::File => {
                if seen_blobs.insert(hash) {
                    let blob_bytes = store.read_object(&ObjectType::Blob, &hash)?;
                    objects.push((ObjectType::Blob, hash, blob_bytes));
                }
            }
            EntryKind::Tree => {
                collect_tree_recursive(store, hash, seen_trees, seen_blobs, objects)?;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum EntryKind {
    File,
    Tree,
}

/// Parse tree entries from tree bytes.
/// Format per entry: type(1) + mode(4) + size(8) + name_len(2) + name(var) + oid(32)
fn parse_tree_entries(bytes: &[u8]) -> Result<Vec<(EntryKind, Hash)>> {
    if bytes.len() < 4 {
        bail!("Tree too short");
    }

    let entry_count = u32::from_le_bytes(bytes[0..4].try_into()?) as usize;
    let mut offset = 4;
    let mut entries = Vec::with_capacity(entry_count);

    for _ in 0..entry_count {
        if offset + 15 > bytes.len() {
            bail!("Tree entry header truncated");
        }

        // Type (1 byte): 0=File, 1=FileExecutable, 2=Tree, 3=Symlink
        let entry_type_byte = bytes[offset];
        let entry_kind = if entry_type_byte == 2 {
            EntryKind::Tree
        } else {
            EntryKind::File // File, FileExecutable, Symlink all point to blobs
        };
        offset += 1;

        // Mode (4 bytes) - skip
        offset += 4;

        // Size (8 bytes) - skip
        offset += 8;

        // Name length (2 bytes)
        let name_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into()?) as usize;
        offset += 2;

        // Name (variable) - skip
        offset += name_len;

        // OID (32 bytes)
        if offset + 32 > bytes.len() {
            bail!("Tree entry OID truncated");
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        entries.push((entry_kind, hash));
    }

    Ok(entries)
}

/// Resolve remote URL and ref name from helix.toml
pub fn resolve_remote_and_ref(
    repo_path: &Path,
    remote_name: &str,
    branch: &str,
) -> Result<(String, String)> {
    let config_path = repo_path.join("helix.toml");

    if !config_path.exists() {
        bail!("Missing helix.toml in repo root. Run `helix init` to initialize a repo");
    }

    let config_text = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let parsed_config: HelixConfig = toml::from_str(&config_text)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    let remotes = parsed_config
        .remotes
        .ok_or_else(|| anyhow::anyhow!("Missing [remotes] section in helix.toml"))?;

    let push_key = format!("{}_push", remote_name);

    let remote_url = remotes.map.get(&push_key).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "Remote '{}' not found. Expected key '{}' in [remotes] table.",
            remote_name,
            push_key,
        )
    })?;

    let ref_name = format!("refs/heads/{branch}");

    Ok((remote_url, ref_name))
}

/// Read a local Helix ref from .helix/refs/<...>
fn read_local_ref(repo_path: &Path, ref_name: &str) -> Result<Hash> {
    let ref_path = repo_path.join(".helix").join(ref_name);

    let hex_contents = fs::read_to_string(&ref_path)
        .with_context(|| format!("Failed to read ref {} ({})", ref_name, ref_path.display()))?;

    hex_to_hash(hex_contents.trim())
}

/// Read remote-tracking ref: .helix/refs/remotes/<remote>/<branch>
pub fn read_remote_tracking(repo_path: &Path, remote: &str, branch: &str) -> Result<Hash> {
    let path = repo_path
        .join(".helix")
        .join("refs")
        .join("remotes")
        .join(remote)
        .join(branch);

    if !path.exists() {
        bail!("Remote-tracking ref does not exist: {}", path.display());
    }

    let hex_contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read remote-tracking ref {}", path.display()))?;

    hex_to_hash(hex_contents.trim())
}

/// Write remote-tracking ref: .helix/refs/remotes/<remote>/<branch>
pub fn write_remote_tracking(
    repo_path: &Path,
    remote: &str,
    branch: &str,
    target: Hash,
) -> Result<()> {
    let path = repo_path
        .join(".helix")
        .join("refs")
        .join("remotes")
        .join(remote);

    fs::create_dir_all(&path).with_context(|| format!("Failed to create {}", path.display()))?;

    let full = path.join(branch);
    let hex = hash_to_hex(&target);
    fs::write(&full, hex + "\n").with_context(|| format!("Failed to write {}", full.display()))?;

    Ok(())
}
