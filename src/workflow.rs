use crate::{config::Config, git::Git, llm::LLM};
use anyhow::{Context, Result};

pub struct Workflow {
    llm: LLM,
    config: Config,
}

impl Workflow {
    pub fn new(config: Config) -> Self {
        let llm = LLM::new(config.clone());
        Self { llm, config }
    }

    /// Complete workflow: add files, generate commit message, commit, and push
    pub async fn auto_commit_and_push(
        &self,
        files: Vec<String>,
        branch: Option<String>,
    ) -> Result<()> {
        println!("🔄 Starting automated commit workflow...");

        // Step 1: Add files to staging
        if !files.is_empty() {
            println!("📁 Adding specified files to staging area...");
            Git::add_files(&files).context("Failed to add files to staging area")?;
        } else {
            // Auto-stage all changes if no specific files provided
            if Git::has_unstaged_changes()? {
                println!("📁 Auto-staging all modified files...");
                Git::add_all_changes().context("Failed to add all changes to staging area")?;
            }
        }

        // Step 2: Check if there are staged changes
        if !Git::has_staged_changes()? {
            return Err(anyhow::anyhow!(
                "No changes found to commit. Make sure you have modified files in your repository."
            ));
        }

        // Step 3: Get the diff
        println!("📊 Reading staged changes...");
        let diff = Git::get_staged_diff().context("Failed to get staged diff")?;

        println!("🤖 Generating commit message with AI...");

        // Step 4: Generate commit message
        let (subject, body) = self
            .llm
            .gen_commit_message(&diff)
            .await
            .context("Failed to generate commit message")?;

        // Step 5: Show the generated message and confirm
        println!("\n📝 Generated commit message:");
        println!("Subject: {}", subject);
        if let Some(ref body_text) = body {
            println!("Body:\n{}", body_text);
        }

        if !self.confirm_commit()? {
            println!("❌ Commit cancelled by user");
            return Ok(());
        }

        // Step 6: Create the commit
        println!("💾 Creating commit...");
        Git::commit(&subject, body.as_deref()).context("Failed to create commit")?;

        // Step 7: Push to remote
        println!("🚀 Pushing to remote...");

        // Use provided branch or get current branch
        let target_branch = match branch {
            Some(b) => b,
            None => Git::get_current_branch_name()
                .context("Failed to get current branch. Specify a branch with --branch")?,
        };

        println!("Pushing to branch: {}", target_branch);
        Git::push(Some(&target_branch)).context("Failed to push to remote")?;

        println!("✅ Workflow completed successfully!");
        Ok(())
    }

    /// Just generate a commit message without committing
    pub async fn generate_message_only(&self) -> Result<()> {
        // Auto-stage changes if nothing is staged
        if !Git::has_staged_changes()? {
            if Git::has_unstaged_changes()? {
                println!("📁 Auto-staging all modified files...");
                Git::add_all_changes()?;
            } else {
                return Err(anyhow::anyhow!(
                    "No changes found. Make sure you have modified files in your repository."
                ));
            }
        }

        println!("📊 Reading staged changes...");
        let diff = Git::get_staged_diff()?;

        println!("🤖 Generating commit message...");
        let (subject, body) = self.llm.gen_commit_message(&diff).await?;

        println!("\n📝 Generated commit message:");
        println!("Subject: {}", subject);
        if let Some(ref body_text) = body {
            println!("Body:\n{}", body_text);
        }

        println!("\nTo use this message:");
        if let Some(body_text) = &body {
            println!("git commit -m \"{}\" -m \"{}\"", subject, body_text);
        } else {
            println!("git commit -m \"{}\"", subject);
        }

        Ok(())
    }

    /// Add files and generate message, but don't commit
    pub async fn stage_and_generate(&self, files: Vec<String>) -> Result<()> {
        if !files.is_empty() {
            println!("📁 Adding specified files to staging area...");
            Git::add_files(&files)?;
        } else {
            if Git::has_unstaged_changes()? {
                println!("📁 Auto-staging all modified files...");
                Git::add_all_changes()?;
            }
        }

        self.generate_message_only().await
    }

    fn confirm_commit(&self) -> Result<bool> {
        use std::io::{self, Write};

        print!("\n❓ Create this commit? [Y/n]: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let input = input.trim().to_lowercase();
        Ok(input.is_empty() || input == "y" || input == "yes")
    }
}
