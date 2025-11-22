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

    // branch to push to (defaults to current branch)
    #[arg(short, long, global = true)]
    branch: Option<String>,

    // add files, generate message, commit, and push automatically
    #[arg(short, long)]
    auto: bool,

    // only generate commit message for staged changes
    #[arg(short, long)]
    generate: bool,

    // add files and generate message (don't commit)
    #[arg(short = 's', long)]
    stage_and_generate: bool,

    // files to add to staging
    files: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Log {
        // repo path (defaults to cwd)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    // show working directory status
    Status {
        // Repository path (defaults to cwd)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    // add files, generate message, commit, and push (default)
    Commit {
        // files to add to staging
        files: Vec<String>,

        // branch to push to (defaults to current branch)
        #[arg(short, long)]
        branch: Option<String>,

        // only generate commit message for staged changes
        #[arg(short, long)]
        generate: bool,

        // add files and generate message (don't commit)
        #[arg(short = 's', long)]
        stage: bool,
    },
    // initialize helix configuration for this repo
    Init {
        // repository path (defaults to cwd)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

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
                workflow
                    .auto_commit_and_push(args.files, args.branch)
                    .await?;
            }
        }
    }

    Ok(())
}

// defaults to cwd
fn resolve_repo_path(path: Option<&Path>) -> Result<PathBuf> {
    let repo_path = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?,
    };

    Ok(repo_path.canonicalize()?)
}

// load config for a repo (merges global + repo config)
fn load_config(repo_path: &Path) -> Result<Config> {
    Config::load(Some(repo_path)).map_err(|e| {
        eprintln!("Failed to load  ~/.helix.toml config: {}", e);
        eprintln!();
        eprintln!("Please create a ~/.helix.toml file by running `helix init`");
        eprintln!();
        e
    })
}

// init repo-specific config
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

    fs::create_dir_all(&helix_dir)?;

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
"#;

    fs::write(&config_path, default_config)?;

    println!("✓ Created .helix/config.toml at {}", config_path.display());
    println!();
    println!(
        "Edit {} to customize this repo's settings",
        config_path.display()
    );
    println!();
    println!("  Settings in .helix/config.toml override ~/.helix.toml");
    println!();
    println!("Next steps:");
    println!("  helix status    # View working directory status");
    println!("  helix log       # View git history");

    Ok(())
}
