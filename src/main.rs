use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod config;
mod git;
mod llm;
mod log;
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Handle subcommands
    match args.command {
        Some(Commands::Log { path }) => {
            log::run(path.as_deref())?;
            return Ok(());
        }
        Some(Commands::Commit {
            files,
            branch,
            generate,
            stage,
        }) => {
            let config = load_config()?;
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
            let config = load_config()?;
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

fn load_config() -> Result<Config> {
    Config::load().map_err(|e| {
        eprintln!("❌ Failed to load config: {}", e);
        eprintln!("Please create a .helix.toml file in your home directory");
        eprintln!("If this is your first time downloading helix, run: `helix init`");
        e
    })
}
