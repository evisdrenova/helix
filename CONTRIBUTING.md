# Contributing to Helix

Thank you for your interest in contributing to Helix! This document provides guidelines and instructions for contributing.

## Development Setup

### Prerequisites

- Rust 1.70+ (install via [rustup](https://rustup.rs/))
- Git (for cloning and version control)

### Building from Source

```bash
# Clone the repository
git clone https://github.com/evisdrenova/helix.git
cd helix

# Build in release mode
cargo build --release

# Binaries will be at:
# - target/release/helix
# - target/release/helix-server
```

### Running Tests

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p helix-cli

# Run a specific test
cargo test test_stage_unstage_workflow
```

### Project Structure

```
helix/
├── helix-cli/          # Main CLI application
│   ├── src/
│   │   ├── main.rs           # CLI entry point
│   │   ├── helix_index/      # Index system (staging, tracking)
│   │   ├── add_command.rs    # helix add
│   │   ├── commit_command.rs # helix commit
│   │   └── ...
│   └── tests/          # Integration tests
├── helix-protocol/     # Binary RPC protocol library
└── helix-server/       # HTTP server for push/pull
```

## Making Changes

### Commit Message Format

We follow conventional commits:

```
type(scope): description

Examples:
feat(checkout): add file deletion support
fix(index): handle empty directories
refactor(commit): simplify HEAD handling
test(add): add blob deduplication test
docs(readme): update installation instructions
```

**Types:** `feat`, `fix`, `refactor`, `test`, `docs`, `chore`

### Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy` and address warnings
- Add tests for new functionality
- Keep functions focused and small

### Testing Guidelines

Tests live in `helix-cli/tests/`. Use the existing patterns:

```rust
use anyhow::Result;
use tempfile::TempDir;

fn init_test_repo(path: &Path) -> Result<()> {
    helix_cli::init_command::init_helix_repo(path, None)?;
    // Set up config...
    Ok(())
}

#[test]
fn test_your_feature() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Your test logic here

    Ok(())
}
```

## Pull Request Process

1. Create a feature branch from `main`
2. Make small, focused commits
3. Ensure all tests pass: `cargo test`
4. Run formatting: `cargo fmt`
5. Run linter: `cargo clippy`
6. Submit PR with clear description

## Getting Help

- Open an issue for bugs or feature requests
- Check existing issues before creating new ones
- For questions, start a discussion in the repository
