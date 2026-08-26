//! Optional LLM assistance via the Anthropic Messages API. Opt-in: enabled only
//! when `ANTHROPIC_API_KEY` is set; the model is overridable with `RGIT_AI_MODEL`.

const DEFAULT_MODEL: &str = "claude-3-5-haiku-latest";
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// Draft a Conventional-Commits message from a staged patch.
pub async fn commit_message(patch: &str) -> Result<String, String> {
    let prompt = format!(
        "Write a Conventional Commits message for this staged diff. The subject \
         must be at most 50 characters; then a blank line and a short body if it \
         helps. Output only the commit message, no preamble or code fences.\n\n{patch}"
    );
    complete(&prompt, 400).await
}

/// Review a diff and return prose findings.
pub async fn code_review(patch: &str) -> Result<String, String> {
    let prompt = format!(
        "Review this diff as a senior engineer. Note real bugs, risks, and \
         missed edge cases; be concise and specific, one finding per line, and \
         say if it looks good. Plain text only.\n\n{patch}"
    );
    complete(&prompt, 1024).await
}

/// One Anthropic Messages API call. Errors (auth, network, response) come back
/// as a string for display; opt-in via `ANTHROPIC_API_KEY`.
async fn complete(prompt: &str, max_tokens: u32) -> Result<String, String> {
    let key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "set ANTHROPIC_API_KEY to use AI features".to_owned())?;
    let model = std::env::var("RGIT_AI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": prompt }],
    });

    let resp = reqwest::Client::new()
        .post(ENDPOINT)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let ok = resp.status().is_success();
    let value: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !ok {
        return Err(value["error"]["message"]
            .as_str()
            .unwrap_or("request failed")
            .to_owned());
    }
    let text = value["content"][0]["text"].as_str().unwrap_or("").trim();
    if text.is_empty() {
        Err("empty response".to_owned())
    } else {
        Ok(text.to_owned())
    }
}
