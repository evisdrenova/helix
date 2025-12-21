use anyhow::{anyhow, bail, Context, Result};
use helix_protocol::{
    read_message, write_message, Hash32, Hello, ObjectType, PushObject, PushRequest, RpcMessage,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::handshake::push_handshake;

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

#[derive(Debug, Deserialize)]
struct HelixToml {
    remotes: Option<RemotesTable>,
}

#[derive(Debug, Deserialize)]
struct RemotesTable {
    #[serde(flatten)]
    map: HashMap<String, String>,
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
                .map(convert_hash32_to_hex32)
                .unwrap_or_else(|| "<none>".to_string())
        );
        println!("  new_target = {}", convert_hash32_to_hex32(&new_target));
    }

    println!("checking if remote server is available..");
    push_handshake(
        &remote_url,
        &repo_path.file_name().unwrap_or_default().to_string_lossy(),
        &ref_name,
        new_target,
        old_target,
    )
    .await?;
    println!("4");
    println!("remote server is available, gathering files to send..");

    if options.dry_run {
        println!("(dry run) Would push from {} to {}", remote_name, branch);
        return Ok(());
    }

    // Compute objects to send (MVP: send everything we have)
    // TODO: update this only send teh difference between the remote and local by walking from new_target back to old_target
    // TODO: as we create the objects, we should write them to the buffer at the same time instead of doing it in sequence
    let objects =
        compute_push_frontier(repo_path, new_target, old_target.unwrap_or([0u8; 32])).await?;
    if options.verbose {
        println!("Sending {} objects...", objects.len());
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
        .await?;

    let status = resp.status();
    let bytes = resp.bytes().await?;

    println!("raw response status = {status}, len = {}", bytes.len());
    // maybe:
    println!("raw response bytes = {:?}", &bytes[..bytes.len().min(64)]);

    println!("as utf8 string: {}", String::from_utf8_lossy(&bytes));

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

/// Resolve remote URL and ref name from helix.toml
pub fn resolve_remote_and_ref(
    repo_path: &Path,
    remote_name: &str,
    branch: &str,
) -> Result<(String, String)> {
    let config_path = repo_path.join("helix.toml");

    if !config_path.exists() {
        bail!("Missing helix.toml in repo rootm run `Helix init` to initialize a repo");
    }

    let config_text = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let parsed_config: HelixToml = toml::from_str(&config_text)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    let remotes = parsed_config
        .remotes
        .ok_or_else(|| anyhow::anyhow!("Missing [remotes] section in helix.toml"))?;

    let push_key = format!("{}_push", remote_name);

    let remote_url = remotes.map.get(&push_key).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "Remote '{}' not found. Expected keys '{}' in [remotes] table.",
            remote_name,
            push_key,
        )
    })?;

    let ref_name = format!("refs/heads/{branch}");

    Ok((remote_url, ref_name))
}

/// Read a local Helix ref from .helix/refs/<...>
/// hash file contents -> 32 raw bytes [0u8, 32]
/// convert to 64 hex bytes and save to disk
/// read hex bytes as strings to memory
/// convert 64 hex bytes back to 32 byte hash string to compare
fn read_local_ref(repo_path: &Path, ref_name: &str) -> Result<Hash32> {
    let ref_path = repo_path.join(".helix").join(ref_name);

    let hex_contents = fs::read_to_string(&ref_path)
        .with_context(|| format!("Failed to read ref {} ({})", ref_name, ref_path.display()))?;

    convert_hex_to_hash32(hex_contents.trim())
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

    let hex_contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read remote-tracking ref {}", path.display()))?;

    convert_hex_to_hash32(hex_contents.trim())
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
    let hex = convert_hash32_to_hex32(&target);
    fs::write(&full, hex + "\n").with_context(|| format!("Failed to write {}", full.display()))?;

    Ok(())
}

/// send all local objects we know about.
async fn compute_push_frontier(
    repo_path: &Path,
    _new_target: Hash32,
    _old_target: Hash32,
) -> Result<Vec<(ObjectType, Hash32, Vec<u8>)>> {
    let objects_root = repo_path.join(".helix").join("objects");

    // Launch all three futures concurrently
    let commits_fut = load_objects_from_dir(objects_root.join("commits"), ObjectType::Commit);
    let trees_fut = load_objects_from_dir(objects_root.join("trees"), ObjectType::Tree);
    let blobs_fut = load_objects_from_dir(objects_root.join("blobs"), ObjectType::Blob);

    // Use try_join! to await them all in parallel
    let (commits, trees, blobs) = tokio::try_join!(commits_fut, trees_fut, blobs_fut)?;

    let mut out = commits;
    out.extend(trees);
    out.extend(blobs);

    Ok(out)
}

async fn load_objects_from_dir(
    dir: PathBuf,
    obj_type: ObjectType,
) -> Result<Vec<(ObjectType, Hash32, Vec<u8>)>> {
    let mut results = Vec::new();
    if !dir.exists() {
        return Ok(results);
    }

    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_file() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let hash = convert_hex_to_hash32(name_str.trim())?;
            let data = tokio::fs::read(entry.path()).await?;
            results.push((obj_type.clone(), hash, data));
        }
    }
    Ok(results)
}

/// converts a hex string to a 32 byte hash
/// this allows us to compare it against other hashes
fn convert_hex_to_hash32(s: &str) -> Result<Hash32> {
    let bytes = hex::decode(s).context("Failed to decode hex string")?;
    bytes.try_into().map_err(|_| anyhow!("Invalid hash length"))
}

/// conevrt a [u8; 32] → hex string
fn convert_hash32_to_hex32(h: &Hash32) -> String {
    hex::encode(h)
}
