use clap::{Parser, Subcommand};
use helix_cli::{
    add_command, branch_command, commit_command,
    init_command::init_helix_repo,
    pull_command::{self, pull},
    push_command::{self, push},
    sandbox_command::{self, CreateOptions},
    unstage_command,
};
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
    branch: Option<String>,
    #[arg(short, long)]
    auto: bool,
    #[arg(short, long)]
    generate: bool,
    #[arg(short = 's', long)]
    stage_and_generate: bool,
    files: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum SandboxCommands {
    /// Create a new sandbox from HEAD (or specified commit)
    Create {
        name: String,
        #[arg(long)]
        base: Option<String>, // Base commit hash (defaults to HEAD)
        #[arg(short, long)]
        verbose: bool,
    },
    /// List all sandboxes
    List {},
    Switch {
        name: String,
    },
    /// Commit sandbox changes
    Commit {
        name: String,
        #[arg(short, long)]
        message: String,
        #[arg(short, long)]
        author: Option<String>,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Merge sandbox commit into a branch
    Merge {
        name: String,
        #[arg(long)]
        into: Option<String>,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Destroy a sandbox
    Destroy {
        name: String,
        #[arg(long)]
        force: bool,
        #[arg(short, long)]
        verbose: bool,
    },
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    Log {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    Status {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    Commit {
        #[arg(short, long)]
        message: Option<String>,
        #[arg(short, long)]
        author: Option<String>,
        #[arg(long)]
        allow_empty: bool,
        #[arg(long)]
        amend: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    Add {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[arg(short, long)]
        verbose: bool,
        #[arg(short = 'n', long)]
        dry_run: bool,
        #[arg(short, long)]
        force: bool,
    },
    /// Unstage files from the staging area
    Unstage {
        /// Files to unstage (use '.' for all staged files)
        #[arg(required = false)]
        paths: Vec<PathBuf>,
        /// Unstage all staged files
        #[arg(short, long)]
        all: bool,
        #[arg(short, long)]
        verbose: bool,
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    Branch {
        name: Option<String>,
        new_name: Option<String>,
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        #[arg(short, long)]
        list: bool,
        #[arg(short, long)]
        delete: bool,
        #[arg(short = 'm', long)]
        rename: bool,
        #[arg(short, long)]
        force: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    Push {
        remote: String,
        branch: String,
        #[arg(short, long)]
        force: bool,
        #[arg(short, long)]
        verbose: bool,
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    Pull {
        remote: String,
        branch: String,
        #[arg(short, long)]
        verbose: bool,
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Manage sandboxes for isolated agent workspaces
    Sandbox {
        #[command(subcommand)]
        command: SandboxCommands,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Some(Commands::Log { path }) => {
            let repo_path = resolve_repo_path(path.as_deref())?;
            log::run(Some(&repo_path))?;
        }
        Some(Commands::Status { path }) => {
            let repo_path = resolve_repo_path(path.as_deref())?;
            status::run(Some(&repo_path))?;
        }
        Some(Commands::Init { path }) => {
            let repo_path = resolve_repo_path(path.as_deref())?;
            init_helix_repo(&repo_path, None)?;
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

            let options = branch_command::BranchOptions {
                delete,
                rename,
                force,
                verbose,
            };

            if (name.is_none() && !delete && !rename) || list {
                branch_command::run_branch_tui(Some(&repo_path))?;
            } else if let Some(branch_name) = name {
                // Handle sandbox branches first (before validation)
                if branch_name.starts_with("sandboxes/") {
                    let sandbox_name = branch_name.strip_prefix("sandboxes/").unwrap();

                    if delete {
                        let destroy_options = sandbox_command::DestroyOptions { force, verbose };
                        sandbox_command::destroy_sandbox(
                            &repo_path,
                            sandbox_name,
                            destroy_options,
                        )?;
                    } else if rename {
                        eprintln!("Error: Cannot rename sandboxes. Destroy and recreate instead.");
                        std::process::exit(1);
                    } else {
                        // Switch to sandbox
                        sandbox_command::switch_sandbox(&repo_path, sandbox_name)?;
                    }
                } else if delete {
                    branch_command::delete_branch(&repo_path, &branch_name, options)?;
                } else if rename {
                    if let Some(new) = new_name {
                        branch_command::rename_branch(&repo_path, &branch_name, &new, options)?;
                    } else {
                        eprintln!("Error: --rename requires two branch names");
                        eprintln!("Usage: helix branch --rename <old-name> <new-name>");
                        std::process::exit(1);
                    }
                } else {
                    // Regular branch - check if exists
                    let branch_exists = repo_path
                        .join(format!(".helix/refs/heads/{}", branch_name))
                        .exists();

                    if branch_exists {
                        branch_command::switch_branch(&repo_path, &branch_name)?;
                    } else {
                        branch_command::create_branch(&repo_path, &branch_name, options)?;
                    }
                }
            } else {
                eprintln!("Error: Branch name required for this operation");
                std::process::exit(1);
            }
        }
        Some(Commands::Add {
            paths,
            verbose,
            dry_run,
            force,
        }) => {
            let repo_path = resolve_repo_path(None)?;

            let options = add_command::AddOptions {
                verbose,
                dry_run,
                force,
            };

            add_command::add(&repo_path, &paths, options)?;
        }
        Some(Commands::Unstage {
            paths,
            all,
            verbose,
            dry_run,
        }) => {
            let repo_path = resolve_repo_path(None)?;

            let options = unstage_command::UnstageOptions { verbose, dry_run };

            if all || paths.is_empty() {
                unstage_command::unstage_all(&repo_path, options)?;
            } else {
                unstage_command::unstage(&repo_path, &paths, options)?;
            }
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
                let options = commit_command::CommitOptions {
                    message: msg,
                    author,
                    allow_empty,
                    amend,
                    verbose,
                };

                commit_command::commit(&repo_path, options)?;
            } else {
                commit_command::show_staged(&repo_path)?;
                eprintln!();
                eprintln!("Aborting commit due to empty commit message.");
                eprintln!("Use 'helix commit -m <message>' to commit.");
                std::process::exit(1);
            }
        }
        Some(Commands::Push {
            remote,
            branch,
            force,
            verbose,
            dry_run,
        }) => {
            let repo_path = resolve_repo_path(None)?;

            let options = push_command::PushOptions {
                verbose,
                dry_run,
                force,
            };

            push(&repo_path, &remote, &branch, options).await?;
        }
        Some(Commands::Pull {
            remote,
            branch,
            verbose,
            dry_run,
        }) => {
            let repo_path = resolve_repo_path(None)?;

            let options = pull_command::PullOptions { verbose, dry_run };

            pull(&repo_path, &remote, &branch, options).await?;
        }
        Some(Commands::Sandbox { command }) => {
            let repo_path = resolve_repo_path(None)?;

            match command {
                SandboxCommands::Create {
                    name,
                    base,
                    verbose,
                } => {
                    let base_commit = match base {
                        Some(hex) => Some(helix_protocol::hash::hex_to_hash(&hex)?),
                        None => None,
                    };

                    let options = CreateOptions {
                        base_commit,
                        verbose,
                    };

                    sandbox_command::create_sandbox(&repo_path, &name, options)?;
                }
                SandboxCommands::List {} => {
                    sandbox_command::run_sandbox_tui(Some(&repo_path))?;
                }
                SandboxCommands::Switch { name } => {
                    sandbox_command::switch_sandbox(&repo_path, &name)?;
                }
                SandboxCommands::Commit {
                    name,
                    message,
                    author,
                    verbose,
                } => {
                    let options = sandbox_command::CommitOptions {
                        message,
                        author,
                        verbose,
                    };
                    sandbox_command::commit_sandbox(&repo_path, &name, options)?;
                }
                SandboxCommands::Merge {
                    name,
                    into,
                    verbose,
                } => {
                    let options = sandbox_command::MergeOptions {
                        into_branch: into,
                        verbose,
                    };
                    sandbox_command::merge_sandbox(&repo_path, &name, options)?;
                }
                SandboxCommands::Destroy {
                    name,
                    force,
                    verbose,
                } => {
                    let options = sandbox_command::DestroyOptions { force, verbose };
                    sandbox_command::destroy_sandbox(&repo_path, &name, options)?;
                }
            }
        }
        None => {
            // Default behavior when no command specified
            println!("Helix - AI-native version control");
            println!();
            println!("Usage: helix <COMMAND>");
            println!();
            println!("Run 'helix --help' for available commands.");
        }
    }

    Ok(())
}

fn resolve_repo_path(path: Option<&Path>) -> Result<PathBuf> {
    let repo_path = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?,
    };

    Ok(repo_path.canonicalize()?)
}
