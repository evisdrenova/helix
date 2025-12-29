use crate::handlers::utils::{handle_handshake, respond_err};
use axum::{extract::State, response::IntoResponse};
use helix_protocol::commit::{parse_commit_for_walk, CommitData};
use helix_protocol::hash::Hash;
use helix_protocol::message::{write_message, ObjectType, PullAck, PullObject, RpcMessage};
use helix_protocol::storage::FsObjectStore;
use helix_server::app_state::AppState;
use std::collections::{HashSet, VecDeque};
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

/// Walk from `from` (remote_head) backwards until we hit `to` (last_known) or run out of parents.
fn walk_commits_between(
    store: &FsObjectStore,
    from: Hash,
    to: Option<Hash>,
) -> anyhow::Result<Vec<CommitData>> {
    let mut result = Vec::new();
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();

    queue.push_back(from);

    while let Some(hash) = queue.pop_front() {
        // Stop if we've reached what client already has
        if to == Some(hash) {
            continue;
        }

        if !seen.insert(hash) {
            continue;
        }

        // Read commit bytes
        let commit_bytes = store.read_object(&ObjectType::Commit, &hash)?;
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
) -> anyhow::Result<Vec<(ObjectType, Hash, Vec<u8>)>> {
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
/// Matches your Tree serialization format from tree.rs.
fn collect_tree_recursive(
    store: &FsObjectStore,
    tree_hash: Hash,
    seen_trees: &mut HashSet<Hash>,
    seen_blobs: &mut HashSet<Hash>,
    objects: &mut Vec<(ObjectType, Hash, Vec<u8>)>,
) -> anyhow::Result<()> {
    if !seen_trees.insert(tree_hash) {
        return Ok(()); // Already processed
    }

    let tree_bytes = store.read_object(&ObjectType::Tree, &tree_hash)?;
    objects.push((ObjectType::Tree, tree_hash, tree_bytes.clone()));

    // Parse tree entries using your format
    let entries = parse_tree_entries(&tree_bytes)?;

    for (entry_type, hash) in entries {
        match entry_type {
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

/// Parse tree entries from your Tree format.
/// Format per entry: type(1) + mode(4) + size(8) + name_len(2) + name(var) + oid(32)
fn parse_tree_entries(bytes: &[u8]) -> anyhow::Result<Vec<(EntryKind, Hash)>> {
    if bytes.len() < 4 {
        anyhow::bail!("Tree too short");
    }

    let entry_count = u32::from_le_bytes(bytes[0..4].try_into()?) as usize;
    let mut offset = 4;
    let mut entries = Vec::with_capacity(entry_count);

    for _ in 0..entry_count {
        if offset + 15 > bytes.len() {
            anyhow::bail!("Tree entry header truncated");
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
            anyhow::bail!("Tree entry OID truncated");
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        entries.push((entry_kind, hash));
    }

    Ok(entries)
}
