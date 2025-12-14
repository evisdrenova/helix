use clap::{Parser, Subcommand};
use helix_cli::{add, branch, commit, init::init_helix_repo, push};
use std::path::{Path, PathBuf};

mod config;
mod git;

mod llm;
mod log;
mod status;
mod workflow;

use anyhow::Result;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(short, long, global = true)]
    branch: Option<String>, // branch to push to (defaults to current branch)
    #[arg(short, long)]
    auto: bool, // add files, generate message, commit, and push automatically
    #[arg(short, long)]
    generate: bool, // only generate commit message for staged changes
    #[arg(short = 's', long)]
    stage_and_generate: bool, // add files and generate message (don't commit)
    files: Vec<String>, // files to add to staging
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>, // repository path (defaults to cwd)
    },
    Log {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>, // repo path (defaults to cwd)v
    },
    Status {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>, // Repository path (defaults to cwd)
    },
    Commit {
        /// Commit message
        #[arg(short, long)]
        message: Option<String>,
        /// Author (overrides config)
        #[arg(short, long)]
        author: Option<String>,
        /// Allow empty commit (no staged files)
        #[arg(long)]
        allow_empty: bool,
        /// Amend previous commit
        #[arg(long)]
        amend: bool,
        /// Show verbose output
        #[arg(short, long)]
        verbose: bool,
    },
    Add {
        #[arg(required = true)]
        paths: Vec<PathBuf>, // Files or directories to add
        #[arg(short, long)]
        verbose: bool, // Show verbose output
        #[arg(short = 'n', long)]
        dry_run: bool, // Perform a dry run (don't actually add)
        #[arg(short, long)]
        force: bool, // Force add (even if in .gitignore)
    },
    Branch {
        name: Option<String>, // Branch name (for create/delete/rename/switch operations)
        new_name: Option<String>, // New branch name (for rename operation)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>, // repository path (defaults to cwd)
        #[arg(short, long)]
        list: bool, // list branches (opens TUI)
        #[arg(short, long)]
        delete: bool, // delete a branch
        #[arg(short = 'm', long)]
        rename: bool, // rename a branch
        #[arg(short, long)]
        force: bool, // force operations
        #[arg(short, long)]
        verbose: bool, // verbose output
    },
    Push {
        /// Remote name (e.g., "origin")
        remote: String,
        /// Branch name (e.g., "main")
        branch: String,
        /// Force push
        #[arg(short, long)]
        force: bool,
        /// Show verbose output
        #[arg(short, long)]
        verbose: bool,
        /// Dry run (show what would be pushed)
        #[arg(short = 'n', long)]
        dry_run: bool,
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
            init_helix_repo(&repo_path, None)?;
            return Ok(());
        }
        Some(Commands::Branch {
            name,
            new_name,
            path,
            list,
            delete,
            rename,
            force,
            verbose,
        }) => {
            let repo_path = resolve_repo_path(path.as_deref())?;

            let options = branch::BranchOptions {
                delete,
                rename,
                force,
                verbose,
            };

            // If no name and no flags, or explicit --list, show TUI
            if (name.is_none() && !delete && !rename) || list {
                branch::run_branch_tui(Some(&repo_path))?;
            } else if let Some(branch_name) = name {
                // Handle specific branch operations
                if delete {
                    // Delete branch
                    branch::delete_branch(&repo_path, &branch_name, options)?;
                } else if rename {
                    // Rename branch
                    if let Some(new) = new_name {
                        branch::rename_branch(&repo_path, &branch_name, &new, options)?;
                    } else {
                        eprintln!("Error: --rename requires two branch names");
                        eprintln!("Usage: helix branch --rename <old-name> <new-name>");
                        std::process::exit(1);
                    }
                } else {
                    // Create or switch to branch
                    let branch_exists = repo_path
                        .join(format!(".helix/refs/heads/{}", branch_name))
                        .exists();

                    if branch_exists {
                        branch::switch_branch(&repo_path, &branch_name)?;
                    } else {
                        branch::create_branch(&repo_path, &branch_name, options)?;
                    }
                }
            } else {
                eprintln!("Error: Branch name required for this operation");
                std::process::exit(1);
            }

            return Ok(());
        }

        Some(Commands::Add {
            paths,
            verbose,
            dry_run,
            force,
        }) => {
            let repo_path = resolve_repo_path(None)?;

            let options = add::AddOptions {
                verbose,
                dry_run,
                force,
            };

            add::add(&repo_path, &paths, options)?;
            return Ok(());
        }
        Some(Commands::Commit {
            message,
            author,
            allow_empty,
            amend,
            verbose,
        }) => {
            let repo_path = resolve_repo_path(None)?;

            if let Some(msg) = message {
                // Commit with message
                let options = commit::CommitOptions {
                    message: msg,
                    author,
                    allow_empty,
                    amend,
                    verbose,
                };

                commit::commit(&repo_path, options)?;
            } else {
                // No message provided - show what would be committed
                commit::show_staged(&repo_path)?;
                eprintln!();
                eprintln!("Aborting commit due to empty commit message.");
                eprintln!("Use 'helix commit -m <message>' to commit.");
                std::process::exit(1);
            }

            return Ok(());
        }
        Some(Commands::Push {
            remote,
            branch,
            force,
            verbose,
            dry_run,
        }) => {
            let repo_path = resolve_repo_path(None)?;

            let options = push::PushOptions {
                verbose,
                dry_run,
                force,
            };

            push::push(&repo_path, &remote, &branch, options)?;
            return Ok(());
        }
        None => {
            // let config = load_config()?;
            // let workflow = Workflow::new(config);

            // if args.auto {
            //     workflow
            //         .auto_commit_and_push(args.files, args.branch)
            //         .await?;
            // } else if args.generate {
            //     if !args.files.is_empty() {
            //         eprintln!("Warning: Files specified with --generate will be ignored");
            //     }
            //     workflow.generate_message_only().await?;
            // } else if args.stage_and_generate {
            //     workflow.stage_and_generate(args.files).await?;
            // } else {
            //     workflow
            //         .auto_commit_and_push(args.files, args.branch)
            //         .await?;
            // }
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
