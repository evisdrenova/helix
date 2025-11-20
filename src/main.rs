use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

mod config;
mod git;
mod llm;
mod log;
mod status;
mod workflow;

use anyhow::Result;
use config::Config;
use workflow::Workflow;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    // Legacy flags for backward compatibility (when no subcommand is used)
    #[arg(short, long, global = true)]
    dry_run: bool,

    /// Branch to push to (defaults to current branch)
    #[arg(short, long, global = true)]
    branch: Option<String>,

    /// Add files, generate message, commit, and push automatically
    #[arg(short, long)]
    auto: bool,

    /// Only generate commit message for staged changes
    #[arg(short, long)]
    generate: bool,

    /// Add files and generate message (don't commit)
    #[arg(short = 's', long)]
    stage_and_generate: bool,

    /// Files to add to staging
    files: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// View beautiful git history
    Log {
        /// Repository path (defaults to current directory)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Show working directory status with FSMonitor
    Status {
        /// Repository path (defaults to current directory)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Add files, generate message, commit, and push (default)
    Commit {
        /// Files to add to staging
        files: Vec<String>,

        /// Branch to push to (defaults to current branch)
        #[arg(short, long)]
        branch: Option<String>,

        /// Only generate commit message for staged changes
        #[arg(short, long)]
        generate: bool,

        /// Add files and generate message (don't commit)
        #[arg(short = 's', long)]
        stage: bool,
    },
    /// Initialize helix configuration for this repository
    Init {
        /// Repository path (defaults to current directory)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Handle subcommands
    match args.command {
        Some(Commands::Log { path }) => {
            let repo_path = resolve_repo_path(path.as_deref())?;
            log::run(Some(&repo_path))?;
            return Ok(());
        }
        Some(Commands::Status { path }) => {
            let repo_path = resolve_repo_path(path.as_deref())?;
            status::run(Some(&repo_path))?;
            return Ok(());
        }
        Some(Commands::Init { path }) => {
            let repo_path = resolve_repo_path(path.as_deref())?;
            init_repo_config(&repo_path)?;
            return Ok(());
        }
        Some(Commands::Commit {
            files,
            branch,
            generate,
            stage,
        }) => {
            let repo_path = resolve_repo_path(None)?;
            let config = load_config(&repo_path)?;
            let workflow = Workflow::new(config);

            if generate {
                if !files.is_empty() {
                    eprintln!("Warning: Files specified with --generate will be ignored");
                }
                workflow.generate_message_only().await?;
            } else if stage {
                workflow.stage_and_generate(files).await?;
            } else {
                workflow.auto_commit_and_push(files, branch).await?;
            }
        }
        None => {
            let repo_path = resolve_repo_path(None)?;
            let config = load_config(&repo_path)?;
            let workflow = Workflow::new(config);

            if args.auto {
                workflow
                    .auto_commit_and_push(args.files, args.branch)
                    .await?;
            } else if args.generate {
                if !args.files.is_empty() {
                    eprintln!("Warning: Files specified with --generate will be ignored");
                }
                workflow.generate_message_only().await?;
            } else if args.stage_and_generate {
                workflow.stage_and_generate(args.files).await?;
            } else {
                // default behavior if no flags provided
                workflow
                    .auto_commit_and_push(args.files, args.branch)
                    .await?;
            }
        }
    }

    Ok(())
}

/// Resolve repository path, defaulting to current directory
fn resolve_repo_path(path: Option<&Path>) -> Result<PathBuf> {
    let repo_path = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?,
    };

    Ok(repo_path.canonicalize()?)
}

/// Load configuration for a repository (merges global + repo config)
fn load_config(repo_path: &Path) -> Result<Config> {
    Config::load(Some(repo_path)).map_err(|e| {
        eprintln!("❌ Failed to load config: {}", e);
        eprintln!();
        eprintln!("Please create a ~/.helix.toml file with your settings.");
        eprintln!();
        eprintln!("Example ~/.helix.toml:");
        eprintln!("  [user]");
        eprintln!("  name = \"Your Name\"");
        eprintln!("  email = \"you@example.com\"");
        eprintln!();
        eprintln!("  model = \"claude-sonnet-4\"");
        eprintln!("  api_base = \"https://api.anthropic.com\"");
        eprintln!("  api_key = \"sk-ant-...\"");
        eprintln!();
        e
    })
}

/// Initialize repository-specific configuration
fn init_repo_config(repo_path: &Path) -> Result<()> {
    use std::fs;

    let helix_dir = repo_path.join(".helix");
    let config_path = helix_dir.join("config.toml");

    if config_path.exists() {
        println!(
            "✓ .helix/config.toml already exists at {}",
            config_path.display()
        );
        println!();
        println!("To edit: {}", config_path.display());
        return Ok(());
    }

    // Create .helix directory
    fs::create_dir_all(&helix_dir)?;

    // Create default repo config
    let default_config = r#"# Helix repository configuration

[core]
auto_refresh = true
refresh_interval_secs = 2

[ignore]
patterns = [
    "*.log",
    "*.tmp",
    "*.swp",
]
respect_gitignore = true

# Hooks (optional)
# [hooks]
# pre_commit = "./scripts/lint.sh"
# pre_push = "./scripts/test.sh"

# Additional remotes (optional)
# [remote.upstream]
# url = "git@github.com:original/repo.git"
"#;

    fs::write(&config_path, default_config)?;

    println!("✓ Created .helix/config.toml at {}", config_path.display());
    println!();
    println!("Repository-specific configuration initialized!");
    println!();
    println!("You can now:");
    println!(
        "  1. Edit {} to customize this repo's settings",
        config_path.display()
    );
    println!("  2. Commit .helix/config.toml to share with your team");
    println!("  3. Settings in .helix/config.toml override ~/.helix.toml");
    println!();
    println!("Next steps:");
    println!("  helix status    # View working directory status");
    println!("  helix log       # View git history");

    Ok(())
}
