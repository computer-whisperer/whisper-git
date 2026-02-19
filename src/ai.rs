//! AI commit message generation with extensible backend support.
//!
//! Provides `AiProvider` enum dispatch for generating commit messages from staged diffs.
//! Currently supports `claude -p` CLI; adding new backends requires only a new enum variant
//! and a single `generate()` function.

/// Which AI backend to use for commit message generation.
#[derive(Clone, Debug)]
pub enum AiProvider {
    ClaudeCli,
    // Future: AnthropicApi { api_key: String, model: String },
    // Future: OpenAi { api_key: String, model: String },
}

/// Input for AI commit message generation.
pub struct AiRequest {
    pub diff_text: String,
    pub branch: String,
}

/// Parsed AI response with subject and optional body.
pub struct AiResponse {
    pub subject: String,
    pub body: String,
}

impl AiProvider {
    /// Blocking call — run in a background thread.
    pub fn generate_commit_message(&self, request: &AiRequest) -> Result<AiResponse, String> {
        match self {
            AiProvider::ClaudeCli => claude_cli::generate(request),
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            AiProvider::ClaudeCli => "Claude CLI",
        }
    }

    /// Parse a config string into an AiProvider.
    pub fn from_config(s: &str) -> Self {
        match s {
            "claude-cli" => AiProvider::ClaudeCli,
            _ => AiProvider::ClaudeCli, // default fallback
        }
    }
}

mod claude_cli {
    use super::{AiRequest, AiResponse};
    use std::process::Command;

    fn find_claude_binary() -> Option<String> {
        // Check $HOME/.local/bin/claude first
        if let Ok(home) = std::env::var("HOME") {
            let local_path = format!("{}/.local/bin/claude", home);
            if std::path::Path::new(&local_path).exists() {
                return Some(local_path);
            }
        }
        // Fall back to PATH lookup
        if Command::new("which").arg("claude").output()
            .map(|o| o.status.success()).unwrap_or(false)
        {
            return Some("claude".to_string());
        }
        None
    }

    pub fn generate(req: &AiRequest) -> Result<AiResponse, String> {
        let binary = find_claude_binary().ok_or_else(|| {
            "Claude CLI not found. Install it from https://docs.anthropic.com/en/docs/claude-code".to_string()
        })?;

        let prompt = format!(
            "Generate a git commit message for the following staged diff.\n\
             Branch: {}\n\n\
             Rules:\n\
             - Write an imperative subject line, max 72 characters\n\
             - Optionally add a body separated by a blank line for complex changes\n\
             - No markdown formatting, no prefixes like \"feat:\" or \"fix:\"\n\
             - Be concise and specific about what changed\n\
             - Output ONLY the commit message, nothing else\n\n\
             Diff:\n{}",
            req.branch, req.diff_text
        );

        let output = Command::new(&binary)
            .arg("-p")
            .arg(&prompt)
            .arg("--model")
            .arg("haiku")
            .arg("--output-format")
            .arg("json")
            .output()
            .map_err(|e| format!("Failed to run claude: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Claude CLI failed: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse JSON response — claude --output-format json returns {"result": "..."}
        let parsed: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| format!("Failed to parse Claude response: {}", e))?;

        let result_text = parsed.get("result")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Claude response missing 'result' field".to_string())?
            .trim()
            .to_string();

        // Split on first double-newline into subject + body
        let (subject, body) = if let Some(pos) = result_text.find("\n\n") {
            (result_text[..pos].trim().to_string(), result_text[pos+2..].trim().to_string())
        } else {
            (result_text, String::new())
        };

        // Ensure subject doesn't exceed 72 chars
        let subject = if subject.len() > 72 {
            subject[..72].trim_end().to_string()
        } else {
            subject
        };

        Ok(AiResponse { subject, body })
    }
}
