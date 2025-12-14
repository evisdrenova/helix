use crate::config::{Config, MessageLevel};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

pub struct LLM {
    config: Config,
}

impl LLM {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn gen_commit_message(&self, diff: &str) -> Result<(String, Option<String>)> {
        let message_level = &self.config.message_level;

        let style = match message_level {
            MessageLevel::Quiet => "a very brief, one-line",
            MessageLevel::Normal => "a concise subject plus short body",
            MessageLevel::Verbose => "a detailed subject and explanatory body",
        };

        let prompt = format!(
            "Write {} Git commit message for these staged changes. Follow conventional commit format.\n\nChanges:\n{}",
            style, diff
        );

        let response = self
            .call_llm_api(
                "You are a helpful assistant that writes clear, concise Git commit messages following conventional commit format.",
                &prompt,
            )
            .await?;

        self.parse_commit_message(&response)
    }

    async fn call_llm_api(&self, system_prompt: &str, user_prompt: &str) -> Result<String> {
        let client = reqwest::Client::new();

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_prompt.to_string(),
                },
            ],
            max_tokens: 200,
            temperature: 0.3,
        };

        let api_url = format!(
            "{}/v1/chat/completions",
            self.config.api_base.trim_end_matches('/')
        );

        let mut req = client
            .post(&api_url)
            .header("Content-Type", "application/json")
            .json(&request);

        // Add Authorization header only if we have a key
        if let Some(ref key) = self.config.api_key {
            if !key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
        }

        let response = req
            .send()
            .await
            .with_context(|| format!("Failed to send request to LLM at {}", api_url))?;

        if !response.status().is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!("LLM API error ({}", error_text));
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse LLM response")?;

        chat_response
            .choices
            .first()
            .map(|choice| choice.message.content.clone())
            .context("No response from LLM")
    }

    fn parse_commit_message(&self, response: &str) -> Result<(String, Option<String>)> {
        let trimmed = response.trim();

        // Split on first empty line to separate subject from body
        if let Some((subject, body)) = trimmed.split_once("\n\n") {
            let subject = subject.lines().next().unwrap_or(subject).to_string();
            let body = body.trim();

            if body.is_empty() {
                Ok((subject, None))
            } else {
                Ok((subject, Some(body.to_string())))
            }
        } else {
            let subject = trimmed.lines().next().unwrap_or(trimmed).to_string();
            Ok((subject, None))
        }
    }
}
