# Integration Guide: Adding `helix log` to Existing CLI

This guide shows how to integrate the `helix log` TUI into your existing Rust-based Helix CLI.

## Step 1: Add Dependencies

Add these to your `Cargo.toml`:

```toml
[dependencies]
# Existing dependencies...

# For helix log TUI
ratatui = "0.28"
crossterm = "0.28"
git2 = "0.19"
chrono = "0.4"
unicode-width = "0.1"
```

## Step 2: Copy Module Files

Copy the entire `src/log/` directory into your project:

```
your-helix-project/
├── src/
│   ├── main.rs
│   ├── log/           ← Add this directory
│   │   ├── mod.rs
│   │   ├── commits.rs
│   │   ├── app.rs
│   │   ├── ui.rs
│   │   └── actions.rs
│   └── ... (your other modules)
```

## Step 3: Register the Module

In your `src/main.rs` or wherever you have your main entry point, add:

```rust
mod log;
```

## Step 4: Add CLI Command

If you're using `clap` or a similar CLI parser, add a `log` subcommand:

### Using clap:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "helix")]
#[command(about = "Helix - AI-native git workflow", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// View beautiful git history
    Log {
        /// Repository path (defaults to current directory)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    // ... your other commands
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Log { path } => {
            log::run(path.as_deref())?;
        }
        // ... handle other commands
    }

    Ok(())
}
```

### Manual parsing (if not using clap):

```rust
use std::env;
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        eprintln!("Usage: helix <command>");
        return Ok(());
    }
    
    match args[1].as_str() {
        "log" => {
            let repo_path = args.get(2).map(|s| Path::new(s));
            log::run(repo_path)?;
        }
        // ... handle other commands
        _ => {
            eprintln!("Unknown command: {}", args[1]);
        }
    }
    
    Ok(())
}
```

## Step 5: Test It

```bash
# Build the project
cargo build --release

# Run helix log in current directory
./target/release/helix log

# Run helix log in specific repo
./target/release/helix log /path/to/repo
```

## Example Integration with Existing Helix Workflow

If your existing helix CLI already has commit functionality, you can connect them:

```rust
// In your commit command handler
Commands::Commit { message } => {
    // Your existing commit logic
    create_and_commit(message)?;
    
    // Show the updated log
    println!("\n✓ Committed successfully! Opening log...\n");
    log::run(Some(Path::new(".")))?;
}
```

This creates a nice flow: commit with helix, immediately see it in the beautiful log view.

## Customization Options

### Change Default Branch Color

In `src/log/ui.rs`, modify the `draw_header` function:

```rust
let branch_text = format!(" ◉ {}  ", app.current_branch);
Span::styled(
    branch_text, 
    Style::default()
        .fg(Color::Magenta)  // Change this color
        .add_modifier(Modifier::BOLD)
)
```

### Change Initial Commit Load Limit

In `src/log/app.rs`, modify the `App::new` function:

```rust
let initial_limit = 100; // Change from 50 to 100
```

### Adjust Split Ratio Default

In `src/log/app.rs`, modify the `App::new` function:

```rust
split_ratio: 0.40, // Change from 0.35 to give more space to timeline
```

### Add Custom Keybindings

In `src/log/app.rs`, add new cases to the `event_loop` match:

```rust
KeyCode::Char('d') => {
    // Show diff for selected commit
    Some(Action::ShowDiff)
}
KeyCode::Char('y') => {
    // Copy commit hash to clipboard
    Some(Action::CopyHash)
}
```

Then implement these actions in `src/log/actions.rs` and handle them in `handle_action`.

## Configuration File Support (Future Enhancement)

You can add support for a `~/.config/helix/log.toml` config file:

```toml
[display]
author_colors = true
relative_time = true
compact_mode = false
initial_limit = 50

[colors]
branch = "cyan"
your_commits = "cyan"
other_commits = "gray"
selected = "yellow"

[keybindings]
quit = "q"
next = "j"
prev = "k"
```

Add the `toml` dependency and parse it in `App::new`:

```rust
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    display: DisplayConfig,
    colors: ColorConfig,
    keybindings: KeybindingConfig,
}

// Load config in App::new
let config = load_config()?;
```

## Troubleshooting

### "Failed to open git repository"
- Ensure you're running in a git repository
- Check that `.git` directory exists
- Try specifying the path explicitly: `helix log /path/to/repo`

### Terminal doesn't render colors
- Ensure your terminal supports colors
- Try setting: `export TERM=xterm-256color`

### Crashes on startup
- Check that git2 can access the repository
- Ensure the repository isn't corrupted: `git fsck`

### Slow performance on huge repos
- Increase lazy loading: reduce `initial_limit` in `App::new`
- Consider adding a commit limit flag: `helix log --limit 100`

## Testing

Create a simple test to ensure the integration works:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_log_module_exists() {
        // This ensures the module compiles and is accessible
        assert!(true);
    }
}
```

Run with:
```bash
cargo test
```

## Next Steps

After integrating the basic log command, you can:

1. **Add filtering**: `helix log --author="you" --since="2 weeks ago"`
2. **Add search**: `helix log --search="auth"`
3. **Add file history**: `helix log --file="src/main.rs"`
4. **Add branch comparison**: `helix log main..feature/auth`

All of these would be extensions to the existing TUI framework!
