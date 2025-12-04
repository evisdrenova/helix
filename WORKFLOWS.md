# Helix Workflows - Complete Test List

## Phase 1: Core Operations (MVP)

### Repository Initialization

- [x] `helix init` - Fresh directory

  - [x] Create .helix/ directory structure
  - [x] Create empty helix.idx
  - [x] Create HEAD file
  - [x] Create config.toml
  - [x] Create objects/ subdirectories
  - [x] Create refs/ subdirectories

- [ ] `helix init` - Existing Git repo

  - [ ] Import from .git/index (one-time)
  - [ ] Filter out .helix/ files
  - [ ] Detect staged files (index != HEAD)
  - [ ] Detect modified files (working != index)
  - [ ] Preserve Git history
  - [ ] Don't re-import on subsequent inits

- [ ] Configuration

  - [ ] Read global config (~/.helix.toml)
  - [ ] Read local config (.helix/config.toml)
  - [ ] Override priority (local > global)
  - [ ] User name/email from config
  - [ ] User name/email from env vars (HELIX_AUTHOR_NAME, HELIX_AUTHOR_EMAIL)

- [ ] Ignore Files
  - [ ] Read .gitignore
  - [ ] Read .helix/config.toml [ignore] section
  - [ ] Built-in ignore patterns (.git, .helix, target/, node_modules/)
  - [ ] Nested .gitignore files
  - [ ] Negation patterns (!important.log)

### Staging Operations

- [ ] `helix add <file>` - Single file

  - [ ] Hash file with BLAKE3
  - [ ] Write blob to .helix/objects/blobs/
  - [ ] Add to index with TRACKED | STAGED flags
  - [ ] Update index generation counter

- [ ] `helix add <pattern>` - Glob patterns

  - [ ] Add src/\*.rs
  - [ ] Add \*_/_.txt
  - [ ] Add .

- [ ] `helix add -A` - Stage all (tracked + untracked)

  - [ ] Stage modified files
  - [ ] Stage new files
  - [ ] Stage deletions

- [ ] `helix add -u` - Stage tracked files only

  - [ ] Stage modified files
  - [ ] Stage deletions
  - [ ] Don't stage untracked files

- [ ] `helix unstage <file>` - Unstage single file

  - [ ] Remove STAGED flag
  - [ ] Keep TRACKED flag
  - [ ] File remains in index

- [ ] `helix unstage -A` - Unstage all

### Commit Operations

- [ ] `helix commit -m "message"` - Basic commit

  - [ ] Build tree from staged files
  - [ ] Create commit object
  - [ ] Update HEAD
  - [ ] Update current branch ref
  - [ ] Clear STAGED flags
  - [ ] Generate commit hash (BLAKE3)

- [ ] `helix commit` - Interactive (opens editor)

  - [ ] Use EDITOR env var
  - [ ] Use configured editor
  - [ ] Abort on empty message

- [ ] `helix commit --amend` - Amend last commit

  - [ ] Load previous commit
  - [ ] Combine with staged changes
  - [ ] Generate new commit hash
  - [ ] Update refs

- [ ] `helix commit --allow-empty` - Empty commit

- [ ] Initial commit (no parent)
  - [ ] Create commit with no parent
  - [ ] Create branch ref
  - [ ] Update HEAD

### Status Operations

- [ ] `helix status` - TUI

  - [ ] Show current branch
  - [ ] Show staged files
  - [ ] Show modified files
  - [ ] Show untracked files (with working tree scan)
  - [ ] Show deleted files
  - [ ] Respect .gitignore
  - [ ] Auto-refresh every 2 seconds
  - [ ] Manual refresh with 'r'

- [ ] `helix status` - CLI (porcelain)

  - [ ] Machine-readable output
  - [ ] Stable format for scripts

- [ ] Status with filters
  - [ ] Toggle untracked ('t' key)
  - [ ] Filter by type ('f' key)
  - [ ] Show only modified
  - [ ] Show only staged

### Log Operations

- [ ] `helix log` - TUI

  - [ ] Show commit history
  - [ ] Show commit messages
  - [ ] Show commit hashes (short)
  - [ ] Show author/date
  - [ ] Navigate with j/k
  - [ ] Quit with q

- [ ] `helix log --oneline` - Compact format

- [ ] `helix log --graph` - Show branch structure

- [ ] `helix log <file>` - File history

- [ ] `helix log <branch>` - Branch history

### Diff Operations

- [ ] `helix diff` - Working tree vs index

  - [ ] Show added lines
  - [ ] Show removed lines
  - [ ] Show modified files
  - [ ] Color output

- [ ] `helix diff --cached` - Index vs HEAD

- [ ] `helix diff <commit>` - Working tree vs commit

- [ ] `helix diff <commit1> <commit2>` - Compare commits

- [ ] `helix diff <file>` - Single file diff

### Branch Operations

- [ ] `helix branch` - List branches

  - [ ] Show current branch with \*
  - [ ] Alphabetical order

- [ ] `helix branch <name>` - Create branch

  - [ ] Create refs/heads/<name>
  - [ ] Point to current commit
  - [ ] Don't switch to new branch

- [ ] `helix branch -d <name>` - Delete branch

  - [ ] Check if not current branch
  - [ ] Check if merged (safety)
  - [ ] Delete branch file

- [ ] `helix branch -D <name>` - Force delete

- [ ] `helix branch -m <old> <new>` - Rename branch

  - [ ] Move branch file
  - [ ] Update HEAD if renaming current branch

- [ ] `helix checkout <branch>` - Switch branches
  - [ ] Update HEAD
  - [ ] Update working tree (future)
  - [ ] Abort if dirty working tree (future)

---

## Phase 2: File Operations & History

### File Manipulation

- [ ] `helix rm <file>` - Remove file

  - [ ] Delete from working tree
  - [ ] Stage deletion
  - [ ] Update index (set DELETED flag)

- [ ] `helix rm --cached <file>` - Untrack file

  - [ ] Remove from index
  - [ ] Keep in working tree

- [ ] `helix mv <old> <new>` - Move/rename file
  - [ ] Detect as rename (not delete + add)
  - [ ] Update index with new path
  - [ ] Preserve history

### Reset Operations

- [ ] `helix reset` - Unstage all (default: mixed)

- [ ] `helix reset --soft <commit>` - Move HEAD only

  - [ ] Update branch ref
  - [ ] Keep index
  - [ ] Keep working tree

- [ ] `helix reset --mixed <commit>` - Move HEAD + index

  - [ ] Update branch ref
  - [ ] Reset index to commit
  - [ ] Keep working tree

- [ ] `helix reset --hard <commit>` - Move HEAD + index + working tree
  - [ ] Update branch ref
  - [ ] Reset index to commit
  - [ ] Reset working tree to commit
  - [ ] ⚠️ DESTRUCTIVE

### Restore Operations

- [ ] `helix restore <file>` - Discard working tree changes

  - [ ] Restore from index
  - [ ] Keep file tracked

- [ ] `helix restore --staged <file>` - Unstage file

- [ ] `helix restore --source=HEAD <file>` - Restore from commit

### Merge Operations

- [ ] `helix merge <branch>` - Fast-forward merge

  - [ ] Detect fast-forward possibility
  - [ ] Move branch pointer
  - [ ] Update working tree

- [ ] `helix merge <branch>` - Three-way merge

  - [ ] Find common ancestor
  - [ ] Merge changes
  - [ ] Create merge commit (two parents)

- [ ] Conflict detection

  - [ ] Detect conflicting changes
  - [ ] Mark conflicts in index (stage 1/2/3)
  - [ ] Write conflict markers to files

- [ ] Conflict resolution

  - [ ] Edit files manually
  - [ ] `helix add` to mark resolved
  - [ ] `helix commit` to complete merge

- [ ] `helix merge --abort` - Abort merge
  - [ ] Restore pre-merge state
  - [ ] Clean up conflict markers

### Rebase Operations

- [ ] `helix rebase <branch>` - Basic rebase

  - [ ] Find commits to replay
  - [ ] Apply commits one by one
  - [ ] Handle conflicts

- [ ] `helix rebase -i <commit>` - Interactive rebase

  - [ ] pick/reword/edit/squash/fixup/drop
  - [ ] Open editor with commit list

- [ ] `helix rebase --abort` - Abort rebase

- [ ] `helix rebase --continue` - Continue after conflict resolution

### History Manipulation

- [ ] `helix cherry-pick <commit>` - Apply commit

  - [ ] Apply changes from commit
  - [ ] Create new commit
  - [ ] Handle conflicts

- [ ] `helix revert <commit>` - Undo commit
  - [ ] Create inverse commit
  - [ ] Preserve history

---

## Phase 3: Remote Operations

### Remote Management

- [ ] `helix remote add <name> <url>` - Add remote

- [ ] `helix remote -v` - List remotes

- [ ] `helix remote remove <name>` - Remove remote

- [ ] `helix remote rename <old> <new>` - Rename remote

### Clone Operations

- [ ] `helix clone <url>` - Clone repository

  - [ ] Download objects
  - [ ] Create remote refs
  - [ ] Checkout default branch
  - [ ] Set up origin remote

- [ ] `helix clone --depth=1 <url>` - Shallow clone

### Fetch Operations

- [ ] `helix fetch` - Fetch from origin

  - [ ] Download new objects
  - [ ] Update remote refs (refs/remotes/origin/\*)
  - [ ] Don't touch working tree

- [ ] `helix fetch <remote>` - Fetch from specific remote

- [ ] `helix fetch --all` - Fetch from all remotes

### Pull Operations

- [ ] `helix pull` - Fetch + merge

  - [ ] Fetch from upstream
  - [ ] Merge into current branch
  - [ ] Handle conflicts

- [ ] `helix pull --rebase` - Fetch + rebase

### Push Operations

- [ ] `helix push` - Push to upstream

  - [ ] Upload new objects
  - [ ] Update remote ref
  - [ ] Fast-forward only (by default)

- [ ] `helix push -f` - Force push

  - [ ] Overwrite remote history
  - [ ] ⚠️ DANGEROUS

- [ ] `helix push --set-upstream <remote> <branch>` - Set tracking

### Branch Tracking

- [ ] Track remote branches

  - [ ] refs/remotes/origin/main
  - [ ] Show in `helix branch -a`

- [ ] `helix branch --set-upstream-to=<remote>/<branch>`

---

## Phase 4: Advanced Features

### Stash Operations

- [ ] `helix stash` - Stash working tree changes

  - [ ] Save to stash stack
  - [ ] Clean working tree
  - [ ] Include untracked files

- [ ] `helix stash list` - List stashes

- [ ] `helix stash apply` - Apply latest stash

  - [ ] Keep stash in stack

- [ ] `helix stash pop` - Apply and drop stash

- [ ] `helix stash drop` - Remove stash

- [ ] `helix stash show` - Show stash diff

### Tag Operations

- [ ] `helix tag <name>` - Lightweight tag

  - [ ] Point to current commit
  - [ ] Store in refs/tags/

- [ ] `helix tag -a <name> -m "message"` - Annotated tag

  - [ ] Create tag object
  - [ ] Include tagger name/email/date
  - [ ] Include message

- [ ] `helix tag` - List tags

- [ ] `helix tag -d <name>` - Delete tag

- [ ] `helix push --tags` - Push tags to remote

### Detached HEAD

- [ ] `helix checkout <commit>` - Checkout commit (detached HEAD)

  - [ ] Update HEAD to commit hash
  - [ ] Show warning
  - [ ] Allow commits (create orphan branch)

- [ ] Create branch from detached HEAD
  - [ ] `helix branch <name>` while detached
  - [ ] Attach HEAD to new branch

### Submodules (Maybe)

- [ ] `helix submodule add <url> <path>`
- [ ] `helix submodule update`
- [ ] `helix submodule init`

### Worktrees (Maybe)

- [ ] `helix worktree add <path> <branch>`
- [ ] `helix worktree list`
- [ ] `helix worktree remove <path>`

---

## Phase 5: Edge Cases & Error Handling

### Corruption Recovery

- [ ] Corrupted helix.idx

  - [ ] Detect via checksum mismatch
  - [ ] Rebuild from .git/index
  - [ ] Rebuild from objects/

- [ ] Missing objects

  - [ ] Detect missing blobs
  - [ ] Detect missing trees
  - [ ] Detect missing commits
  - [ ] Attempt recovery from Git

- [ ] Corrupted config files
  - [ ] Use defaults
  - [ ] Show warning

### Concurrent Access

- [ ] Multiple helix processes

  - [ ] Lock .helix/index.lock
  - [ ] Wait for lock release
  - [ ] Timeout after 5 seconds

- [ ] Helix + Git interop
  - [ ] Detect .git/index changes
  - [ ] Reload helix.idx
  - [ ] Show warning

### Special Cases

- [ ] Empty repository

  - [ ] No commits
  - [ ] No branches
  - [ ] Handle gracefully

- [ ] Large files

  - [ ] Files > 100MB
  - [ ] Show warning
  - [ ] Consider LFS integration

- [ ] Binary files

  - [ ] Detect binary content
  - [ ] Don't show diff
  - [ ] Store efficiently

- [ ] Symlinks

  - [ ] Store as symlink (mode 120000)
  - [ ] Preserve target
  - [ ] Handle broken symlinks

- [ ] Executable files

  - [ ] Preserve executable bit (mode 100755)
  - [ ] Detect on add
  - [ ] Restore on checkout

- [ ] Case-insensitive filesystems
  - [ ] macOS / Windows
  - [ ] Handle collisions
  - [ ] Warn on case-only renames

### Performance Edge Cases

- [ ] Very large repositories

  - [ ] 100K+ files
  - [ ] 100K+ commits
  - [ ] Deep history

- [ ] Very large files

  - [ ] Multi-GB files
  - [ ] Streaming hashing
  - [ ] Incremental writes

- [ ] Deep directory structures
  - [ ] 1000+ level nesting
  - [ ] Path length limits

---

## Testing Strategy

### Unit Tests

- [ ] Hash module (BLAKE3)
- [ ] Blob storage (compression)
- [ ] Tree building (parallelization)
- [ ] Commit creation
- [ ] Index format serialization
- [ ] Flag manipulation
- [ ] Ignore rules parsing

### Integration Tests

- [ ] Full workflows (init → add → commit → log)
- [ ] Git interop (import → modify → export)
- [ ] Branch workflows (create → switch → merge)
- [ ] Remote workflows (clone → fetch → push)
- [ ] Conflict resolution
- [ ] Error recovery

### Performance Tests

- [ ] Benchmarks for all core operations
- [ ] Comparison with Git
- [ ] Memory usage profiling
- [ ] Scalability tests (10K, 100K, 1M files)

### Compatibility Tests

- [ ] Git interop (read/write)
- [ ] Cross-platform (Linux, macOS, Windows)
- [ ] Different filesystems (ext4, APFS, NTFS)

---

## Documentation

- [ ] User guide
- [ ] Command reference
- [ ] Configuration guide
- [ ] Architecture documentation
- [ ] API documentation
- [ ] Migration guide (from Git)
- [ ] Troubleshooting guide

---

## Priority Checklist

### Must Have (MVP)

- [x] init, add, commit, status, log, branch, checkout, diff
- [ ] rm, mv, reset, restore
- [ ] Working tree scan for untracked files
- [ ] .gitignore support

### Should Have

- [ ] merge, rebase, cherry-pick, revert
- [ ] stash, tags
- [ ] Error recovery
- [ ] Performance optimization

### Nice to Have

- [ ] Remote operations (clone, fetch, pull, push)
- [ ] Submodules, worktrees
- [ ] Advanced Git interop

### Future

- [ ] GUI/TUI
- [ ] IDE integration
- [ ] LFS support
- [ ] Signing (GPG)
