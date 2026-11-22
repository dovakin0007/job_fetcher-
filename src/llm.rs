//! Multi-provider LLM client for resume rewriting.
//!
//! The tailor flow rewrites the parsed resume per job through whichever
//! provider is configured: OpenCode (primary), Ollama (fallback),
//! OpenAI-compatible, Anthropic, or Gemini. Every provider returns JSON;
//! anything that cannot be validated against the parsed resume is discarded
//! by the caller in favour of the deterministic base.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    OpenCode,
    Ollama,
    Openai,
    Anthropic,
    Gemini,
}

impl LlmProvider {
    pub fn name(&self) -> &'static str {
        match self {
            LlmProvider::OpenCode => "opencode",
            LlmProvider::Ollama => "ollama",
            LlmProvider::Openai => "openai",
            LlmProvider::Anthropic => "anthropic",
            LlmProvider::Gemini => "gemini",
        }
    }
}

/// Which wire format an OpenCode model speaks. Fixed per model family:
/// open models (kimi, glm, minimax, …) use the OpenAI chat API, Claude and
/// Qwen use the Anthropic Messages API, Gemini models use the Google path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeFamily {
    OpenAI,
    Anthropic,
    Gemini,
}

/// Reads `OPENCODE_API_FAMILY` (`openai` default, `anthropic`, `gemini`).
pub fn opencode_family() -> OpenCodeFamily {
    match std::env::var("OPENCODE_API_FAMILY")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "anthropic" | "claude" => OpenCodeFamily::Anthropic,
        "gemini" | "google" => OpenCodeFamily::Gemini,
        _ => OpenCodeFamily::OpenAI,
    }
}

/// `(base_url, model, api_key)` when OpenCode inference is configured.
pub fn opencode_config() -> Option<(String, String, String)> {
    let key = std::env::var("OPENCODE_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())?;
    let base = std::env::var("OPENCODE_BASE_URL")
        .unwrap_or_else(|_| "https://opencode.ai/inference".into());
    let model = std::env::var("OPENCODE_MODEL").unwrap_or_else(|_| "kimi-k3".into());
    Some((base, model, key))
}

/// Full request URL for an OpenCode model (pure, unit-tested).
pub fn opencode_endpoint(base: &str, family: OpenCodeFamily, model: &str) -> String {
    let base = base.trim_end_matches('/');
    match family {
        OpenCodeFamily::OpenAI => format!("{base}/openai/v1/chat/completions"),
        OpenCodeFamily::Anthropic => format!("{base}/anthropic/v1/messages"),
        OpenCodeFamily::Gemini => {
            format!("{base}/google/v1beta/models/{model}:generateContent")
        }
    }
}

/// One OpenCode chat call returning parsed JSON. Shared by the tailor flow
/// and resume extraction so both speak the same endpoint contract.
pub async fn opencode_chat_json(
    client: &reqwest::Client,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> anyhow::Result<(String, Value)> {
    let (base, model, key) =
        opencode_config().ok_or_else(|| anyhow::anyhow!("OPENCODE_API_KEY is not set"))?;
    let family = opencode_family();
    let url = opencode_endpoint(&base, family, &model);
    let (body, parse): (Value, fn(&Value) -> Option<String>) = match family {
        OpenCodeFamily::OpenAI => (
            serde_json::json!({
                "model": model,
                "temperature": 0,
                "response_format": { "type": "json_object" },
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user },
                ],
            }),
            |response| {
                response["choices"][0]["message"]["content"]
                    .as_str()
                    .map(str::to_string)
            },
        ),
        OpenCodeFamily::Anthropic => (
            serde_json::json!({
                "model": model,
                "max_tokens": max_tokens,
                "temperature": 0,
                "system": system,
                "messages": [{ "role": "user", "content": user }],
            }),
            |response| response["content"][0]["text"].as_str().map(str::to_string),
        ),
        OpenCodeFamily::Gemini => (
            serde_json::json!({
                "system_instruction": { "parts": [{ "text": system }] },
                "contents": [{ "parts": [{ "text": user }] }],
                "generationConfig": {
                    "temperature": 0,
                    "maxOutputTokens": max_tokens,
                    "responseMimeType": "application/json",
                },
            }),
            |response| {
                response["candidates"][0]["content"]["parts"][0]["text"]
                    .as_str()
                    .map(str::to_string)
            },
        ),
    };
    let response: Value = client
        .post(url)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let text = parse(&response)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("empty opencode response"))?;
    Ok((model, parse_json_object(&text)?))
}

struct Attempt {
    provider: LlmProvider,
    url: String,
    key: Option<String>,
    model: String,
    base_for_openai: Option<String>,
}

/// Providers to try, in order. `TAILOR_LLM_PROVIDER` wins; otherwise the resume
/// provider setting is reused so one variable configures both flows. The
/// default order puts OpenCode first and Ollama last as the offline fallback.
fn wanted_providers() -> Vec<&'static str> {
    let raw = std::env::var("TAILOR_LLM_PROVIDER")
        .or_else(|_| std::env::var("RESUME_LLM_PROVIDER"))
        .unwrap_or_else(|_| "both".into())
        .to_ascii_lowercase();
    if raw == "both" || raw == "all" {
        return vec!["opencode", "openai", "anthropic", "gemini", "ollama"];
    }
    vec![match raw.as_str() {
        "opencode" => "opencode",
        "ollama" => "ollama",
        "openai" => "openai",
        "anthropic" | "claude" => "anthropic",
        "gemini" | "google" => "gemini",
        _ => "ollama",
    }]
}

fn attempts() -> Vec<Attempt> {
    let mut out = Vec::new();
    for name in wanted_providers() {
        match name {
            "opencode" => {
                if let Some((_, model, key)) = opencode_config() {
                    out.push(Attempt {
                        provider: LlmProvider::OpenCode,
                        url: String::new(),
                        key: Some(key),
                        model,
                        base_for_openai: None,
                    });
                }
            }
            "ollama" => {
                if let Ok(model) =
                    std::env::var("OLLAMA_MODEL").or_else(|_| std::env::var("LLM_MODEL"))
                {
                    let base = std::env::var("OLLAMA_BASE_URL")
                        .or_else(|_| std::env::var("LLM_BASE_URL"))
                        .unwrap_or_else(|_| "http://localhost:11434".into());
                    out.push(Attempt {
                        provider: LlmProvider::Ollama,
                        url: format!("{}/api/chat", base.trim_end_matches('/')),
                        key: None,
                        model,
                        base_for_openai: None,
                    });
                }
            }
            "openai" => {
                if let (Ok(model), Ok(key)) = (
                    std::env::var("OPENAI_MODEL"),
                    std::env::var("OPENAI_API_KEY"),
                ) {
                    let base = std::env::var("OPENAI_BASE_URL")
                        .unwrap_or_else(|_| "https://api.openai.com".into());
                    out.push(Attempt {
                        provider: LlmProvider::Openai,
                        url: String::new(),
                        key: Some(key),
                        model,
                        base_for_openai: Some(base),
                    });
                }
            }
            "anthropic" => {
                if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                    let model = std::env::var("ANTHROPIC_MODEL")
                        .or_else(|_| std::env::var("CLAUDE_MODEL"))
                        .unwrap_or_else(|_| "claude-sonnet-4-5".into());
                    out.push(Attempt {
                        provider: LlmProvider::Anthropic,
                        url: "https://api.anthropic.com/v1/messages".into(),
                        key: Some(key),
                        model,
                        base_for_openai: None,
                    });
                }
            }
            "gemini" => {
                if let Ok(key) = std::env::var("GEMINI_API_KEY") {
                    let model =
                        std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.0-flash".into());
                    out.push(Attempt {
                        provider: LlmProvider::Gemini,
                        url: format!(
                            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
                        ),
                        key: Some(key),
                        model,
                        base_for_openai: None,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Sends the prompts to each configured provider in order and returns the
/// first valid JSON object plus which provider produced it.
pub async fn chat_json(system: &str, user: &str) -> anyhow::Result<(LlmProvider, Value)> {
    let attempts = attempts();
    if attempts.is_empty() {
        anyhow::bail!(
            "No tailor LLM configured. Recommended: OPENCODE_API_KEY + OPENCODE_MODEL \
             (e.g. kimi-k3) with TAILOR_LLM_PROVIDER=opencode, falling back to Ollama. \
             Alternatives: OLLAMA_MODEL (+OLLAMA_BASE_URL), OPENAI_MODEL+OPENAI_API_KEY, \
             ANTHROPIC_API_KEY (+ANTHROPIC_MODEL), or GEMINI_API_KEY (+GEMINI_MODEL)."
        );
    }
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(tailor_timeout_secs()))
        .build()?;
    let mut errors = Vec::new();
    for attempt in attempts {
        match call_once(&client, &attempt, system, user).await {
            Ok(value) => return Ok((attempt.provider, value)),
            Err(error) => errors.push(format!("{}: {error:#}", attempt.provider.name())),
        }
    }
    anyhow::bail!("All tailor LLM providers failed: {}", errors.join("; "))
}

/// Total HTTP timeout per tailor attempt. Local models are slow: a ~10B
/// quant needs ~1-2 min for a short rewrite. `TAILOR_LLM_TIMEOUT_SECS`
/// overrides the 180s default; connect failures still fail fast (10s).
fn tailor_timeout_secs() -> u64 {
    std::env::var("TAILOR_LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(180)
}
async fn call_once(
    client: &reqwest::Client,
    attempt: &Attempt,
    system: &str,
    user: &str,
) -> anyhow::Result<Value> {
    let text = match attempt.provider {
        LlmProvider::OpenCode => {
            let (_, value) = opencode_chat_json(client, system, user, 4096).await?;
            return Ok(value);
        }
        LlmProvider::Ollama => {
            let body = serde_json::json!({
                "model": attempt.model,
                "stream": false,
                "think": false,
                "format": "json",
                "options": { "temperature": 0, "num_predict": 1600, "num_ctx": 8192 },
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user },
                ],
            });
            let response: Value = client
                .post(&attempt.url)
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            response["message"]["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("empty ollama response"))?
                .to_string()
        }
        LlmProvider::Openai => {
            let base = attempt
                .base_for_openai
                .as_deref()
                .unwrap_or("https://api.openai.com");
            let base = base.trim_end_matches('/');
            let endpoint = if base.ends_with("/v1") {
                format!("{base}/chat/completions")
            } else {
                format!("{base}/v1/chat/completions")
            };
            let body = serde_json::json!({
                "model": attempt.model,
                "temperature": 0,
                "response_format": { "type": "json_object" },
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user },
                ],
            });
            let mut request = client.post(endpoint).json(&body);
            if let Some(key) = attempt.key.as_deref() {
                request = request.bearer_auth(key);
            }
            let response: Value = request.send().await?.error_for_status()?.json().await?;
            response["choices"][0]["message"]["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("empty openai-compatible response"))?
                .to_string()
        }
        LlmProvider::Anthropic => {
            let body = serde_json::json!({
                "model": attempt.model,
                "max_tokens": 4096,
                "temperature": 0,
                "system": system,
                "messages": [{ "role": "user", "content": user }],
            });
            let response: Value = client
                .post(&attempt.url)
                .header("x-api-key", attempt.key.as_deref().unwrap_or_default())
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            response["content"][0]["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("empty anthropic response"))?
                .to_string()
        }
        LlmProvider::Gemini => {
            let body = serde_json::json!({
                "system_instruction": { "parts": [{ "text": system }] },
                "contents": [{ "parts": [{ "text": user }] }],
                "generationConfig": { "temperature": 0, "responseMimeType": "application/json" },
            });
            let response: Value = client
                .post(&attempt.url)
                .query(&[("key", attempt.key.as_deref().unwrap_or_default())])
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            response["candidates"][0]["content"]["parts"][0]["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("empty gemini response"))?
                .to_string()
        }
    };
    Ok(parse_json_object(&text)?)
}

fn parse_json_object(text: &str) -> anyhow::Result<Value> {
    let trimmed = text.trim();
    // Strip markdown code fences when the model adds them despite instructions.
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    let value: Value = serde_json::from_str(inner)?;
    if !value.is_object() {
        anyhow::bail!("LLM did not return a JSON object");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_code_fences() {
        let v = parse_json_object("```json\n{\"a\":1}\n```").unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn rejects_non_objects() {
        assert!(parse_json_object("[1,2]").is_err());
    }

    #[test]
    fn opencode_endpoints_match_the_docs() {
        let base = "https://opencode.ai/inference";
        assert_eq!(
            opencode_endpoint(base, OpenCodeFamily::OpenAI, "kimi-k3"),
            "https://opencode.ai/inference/openai/v1/chat/completions"
        );
        assert_eq!(
            opencode_endpoint(base, OpenCodeFamily::Anthropic, "claude-sonnet-5-5"),
            "https://opencode.ai/inference/anthropic/v1/messages"
        );
        assert_eq!(
            opencode_endpoint(base, OpenCodeFamily::Gemini, "gemini-4-argon"),
            "https://opencode.ai/inference/google/v1beta/models/gemini-4-argon:generateContent"
        );
        assert_eq!(
            opencode_endpoint("https://proxy.local/", OpenCodeFamily::OpenAI, "m"),
            "https://proxy.local/openai/v1/chat/completions"
        );
    }

    #[test]
    fn opencode_family_defaults_to_openai() {
        std::env::remove_var("OPENCODE_API_FAMILY");
        assert_eq!(opencode_family(), OpenCodeFamily::OpenAI);
        std::env::set_var("OPENCODE_API_FAMILY", "anthropic");
        assert_eq!(opencode_family(), OpenCodeFamily::Anthropic);
        std::env::set_var("OPENCODE_API_FAMILY", "gemini");
        assert_eq!(opencode_family(), OpenCodeFamily::Gemini);
        std::env::remove_var("OPENCODE_API_FAMILY");
    }
}
