# **Helix — A Next-Generation Version Control System**

(UNDER ACTIVE DEVELOPMENT)

Helix is a high-performance version control system designed for the AI-native developer workflow. Helix optimizes for **CPU parallelism**, **modern storage backends**, **LLM-generated code volume**, and **low-latency developer workflows**.

Helix is built around a memory-mapped index, a parallel tree builder, BLAKE3 hashing, and efficient Zstd-compressed objects.

Benchmarks show **20–100× speedups** over Git for operations like:

- `helix add` on large directories
- Building trees
- Computing commits
- Reading and writing the index

Helix is designed to keep latency _flat_ even as repos grow.

Unlike other VCSs that wrap Git, Helix is a new VCS with a clean architecture, aggressively optimized primitives, and a modern push/pull protocol.

You can import a Git repository, but Helix stores and transmits data in its _own_ native format.

# **Architecture**

```
+-------------------+          +----------------+
|  Helix CLI        |          | Helix Server   |
|-------------------|   RPC    |----------------|
| add/status/commit | <------> | push/fetch     |
| build tree        |          | store objects  |
| compute hashes    |          | update refs    |
+-------------------+          +----------------+

```

Helix uses three core object types — **blobs, trees, commits** — stored in a new, compact, content-addressed format under:

```
.helix/
  objects/
    blobs/
    trees/
    commits/
  refs/
    heads/
    remotes/
  helix.idx   (memory-mapped index)
```

### **Push/Pull Protocol**

Helix defines a custom RPC protocol optimized for speed and avoids Git's pack negotiation:

- Binary, streaming frames
- Zero round-trip negotiation
- Efficient object transfer (commit/tree/blob)
- Server implemented with Axum (Rust)
- CLI sends objects incrementally, server responds with structured ACKs

### **Extendability**

Helix’s storage and index layers are intentionally simple and viewable:

- Memory-mapped index file → instant load
- Hash format: 32-byte BLAKE3 digests
- Trees and commits stored uncompressed
- Zstd-compressed blobs
- Entire object store is filesystem-native (no packfile management)

This makes Helix ideal for:

- AI-generated code workflows
- Massive monorepos
- Programmatic manipulation of history

# **Project Status**

Helix is **experimental** and under active development.

Working today:

- Local VCS operations (status, add, commit, log)
- Commit/tree/blob storage
- Branch + HEAD management
- Git → Helix importer
- Push/pull with a running Helix server
- TUI

Coming next:

- Merge engine
- Diffs + patch application
- Conflict resolution
- Smarter remote negotiation
- Authentication
- Multi-repo hosting
- GUI + improved TUI

---

# Getting Started

```sh
# Start server
HELIX_REPO_ROOT=/tmp/helix-server-data helix-server

# In another directory
helix init
echo "hello" > file.txt
helix add file.txt
helix commit -m "first commit"

# Configure helix.toml
[remotes]
origin_push = "http://127.0.0.1:8080"

# Push
helix push origin main
```

# Contributing

Helix welcomes contributors interested in:

- High-performance Rust
- Version control internals
- Storage engines
- Network protocol design
- UI/UX for developer tools
