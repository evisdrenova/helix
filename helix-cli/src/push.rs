use anyhow::{anyhow, bail, Context, Result};
use helix_protocol::{
    read_message, write_message, Hash32, Hello, ObjectType, PushObject, PushRequest, RpcMessage,
};
use ini::Ini;
use serde::Deserialize;
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

pub async fn push(
    repo_path: &Path,
    remote: &str,
    branch: &str,
    options: PushOptions,
) -> Result<()> {
    let repo_path = std::env::current_dir()?;
    let remote_name = remote;
    let helix_dir = repo_path.join(".helix");
    if !helix_dir.exists() {
        bail!("Not a Helix repo (no .helix directory)");
    }

    // 1. Resolve remote URL & ref name
    let (remote_url, ref_name) = resolve_remote_and_ref(&repo_path, remote_name, branch)?;

    // 2. Find local branch HEAD
    let new_target =
        read_local_ref(&repo_path, &ref_name).context("Failed to read local branch head")?;

    // 3. Determine old_target (from remote-tracking ref)
    let old_target = read_remote_tracking(&repo_path, remote_name, branch).unwrap_or([0u8; 32]); // ZERO_HASH

    if options.verbose {
        println!("Pushing {ref_name} to {remote_name} at {remote_url}");
        println!("  old_target = {}", hash_to_hex32(&old_target));
        println!("  new_target = {}", hash_to_hex32(&new_target));
    }

    if options.dry_run {
        println!("(dry run) Would push from {} to {}", remote_name, branch);
        return Ok(());
    }

    // 4. Compute objects to send (MVP: send everything we have)
    let objects = compute_push_frontier(&repo_path, new_target, old_target)?;

    if options.verbose {
        println!("Sending {} objects...", objects.len());
    }

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
                .unwrap_or_default()
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

#[derive(Debug, Deserialize)]
struct HelixToml {
    remotes: Option<RemotesTable>,
}

#[derive(Debug, Deserialize)]
struct RemotesTable {
    #[serde(flatten)]
    map: std::collections::HashMap<String, String>,
}

/// Resolve remote URL and ref name from helix.toml (INI)
pub fn resolve_remote_and_ref(
    repo_path: &Path,
    remote_name: &str,
    branch: &str,
) -> Result<(String, String)> {
    let config_path = repo_path.join("helix.toml");

    if !config_path.exists() {
        bail!(
            "Missing helix.toml in repo root (needed to resolve remote '{}')",
            remote_name
        );
    }

    // Read file
    let text = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    // Parse TOML to struct
    let parsed: HelixToml = toml::from_str(&text)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    let remotes = parsed
        .remotes
        .ok_or_else(|| anyhow::anyhow!("Missing [remotes] section in helix.toml"))?;

    // Look up keys:
    //   origin_push
    //   origin_pull
    let push_key = format!("{}_push", remote_name);
    let pull_key = format!("{}_pull", remote_name);

    // Select push→pull priority
    let remote_url = remotes
        .map
        .get(&push_key)
        .cloned()
        .or_else(|| remotes.map.get(&pull_key).cloned())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Remote '{}' not found. Expected keys '{}' or '{}' in [remotes] table.",
                remote_name,
                push_key,
                pull_key
            )
        })?;

    // Construct ref name
    let ref_name = format!("refs/heads/{branch}");

    Ok((remote_url, ref_name))
}

/// Read a local Helix ref from .helix/refs/<...>
fn read_local_ref(repo_path: &Path, ref_name: &str) -> Result<Hash32> {
    let ref_path = repo_path.join(".helix").join(ref_name);

    let hex = fs::read_to_string(&ref_path)
        .with_context(|| format!("Failed to read ref {} ({})", ref_name, ref_path.display()))?;

    hex_to_hash32(hex.trim())
}

/// Read remote-tracking ref: .helix/refs/remotes/<remote>/<branch>
pub fn read_remote_tracking(repo_path: &Path, remote: &str, branch: &str) -> Result<Hash32> {
    let path = repo_path
        .join(".helix")
        .join("refs")
        .join("remotes")
        .join(remote)
        .join(branch);

    if !path.exists() {
        bail!("Remote-tracking ref does not exist: {}", path.display());
    }

    let hex = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read remote-tracking ref {}", path.display()))?;

    hex_to_hash32(hex.trim())
}

/// Write remote-tracking ref: .helix/refs/remotes/<remote>/<branch>
pub fn write_remote_tracking(
    repo_path: &Path,
    remote: &str,
    branch: &str,
    target: Hash32,
) -> Result<()> {
    let path = repo_path
        .join(".helix")
        .join("refs")
        .join("remotes")
        .join(remote);

    fs::create_dir_all(&path).with_context(|| format!("Failed to create {}", path.display()))?;

    let full = path.join(branch);
    let hex = hash_to_hex32(&target);
    fs::write(&full, hex + "\n").with_context(|| format!("Failed to write {}", full.display()))?;

    Ok(())
}

/// MVP: compute push frontier by sending **all** local objects we know about.
///
/// Later we can:
///   - walk commits from new_target back to old_target
///   - only send the closure of reachable commits/trees/blobs
fn compute_push_frontier(
    repo_path: &Path,
    _new_target: Hash32,
    _old_target: Hash32,
) -> Result<Vec<(ObjectType, Hash32, Vec<u8>)>> {
    let objects_root = repo_path.join(".helix").join("objects");

    let mut out: Vec<(ObjectType, Hash32, Vec<u8>)> = Vec::new();

    // Commits
    let commits_dir = objects_root.join("commits");
    if commits_dir.exists() {
        for entry in fs::read_dir(&commits_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let hash = hex_to_hash32(name.trim())?;
            let data = fs::read(entry.path())?;
            out.push((ObjectType::Commit, hash, data));
        }
    }

    // Trees
    let trees_dir = objects_root.join("trees");
    if trees_dir.exists() {
        for entry in fs::read_dir(&trees_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let hash = hex_to_hash32(name.trim())?;
            let data = fs::read(entry.path())?;
            out.push((ObjectType::Tree, hash, data));
        }
    }

    // Blobs
    let blobs_dir = objects_root.join("blobs");
    if blobs_dir.exists() {
        for entry in fs::read_dir(&blobs_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let hash = hex_to_hash32(name.trim())?;
            let data = fs::read(entry.path())?;
            out.push((ObjectType::Blob, hash, data));
        }
    }

    Ok(out)
}

/// Local helper: hex string → [u8; 32]
fn hex_to_hash32(s: &str) -> Result<Hash32> {
    use std::num::ParseIntError;

    if s.len() != 64 {
        bail!("Invalid hash length {}, expected 64 hex chars", s.len());
    }

    let mut out = [0u8; 32];
    for i in 0..32 {
        let byte_str = &s[i * 2..i * 2 + 2];
        let byte = u8::from_str_radix(byte_str, 16)
            .map_err(|e: ParseIntError| anyhow!("Invalid hex byte '{}': {}", byte_str, e))?;
        out[i] = byte;
    }
    Ok(out)
}

/// Local helper: [u8; 32] → hex string
fn hash_to_hex32(h: &Hash32) -> String {
    h.iter().map(|b| format!("{:02x}", b)).collect()
}
