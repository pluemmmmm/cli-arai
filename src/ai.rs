use anyhow::{anyhow, Context, Result};
use serde_json::json;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderKind {
    ApiKey,
    LocalCli,
    LocalService,
}

#[derive(Debug, Clone)]
pub struct AiProvider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub detail: String,
    pub available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl EffortLevel {
    pub fn all() -> &'static [Self] {
        &[Self::Low, Self::Medium, Self::High, Self::XHigh, Self::Max]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::XHigh => "XHigh",
            Self::Max => "Max",
        }
    }

    pub fn api_value(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiRequestOptions {
    pub model: Option<String>,
    pub agent: Option<String>,
    pub effort: EffortLevel,
    pub mode: Option<String>,
    pub system_instruction: Option<String>,
    pub timeout_secs: u64,
    pub attachments: Vec<PathBuf>,
}

impl Default for AiRequestOptions {
    fn default() -> Self {
        Self {
            model: None,
            agent: None,
            effort: EffortLevel::Medium,
            mode: None,
            system_instruction: None,
            timeout_secs: 0,
            attachments: Vec::new(),
        }
    }
}

fn mime_for_image(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "image/png",
    }
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn load_encoded_images(paths: &[PathBuf]) -> Vec<(&'static str, String)> {
    paths
        .iter()
        .filter_map(|p| {
            let bytes = std::fs::read(p).ok()?;
            Some((mime_for_image(p), encode_base64(&bytes)))
        })
        .collect()
}

/// Split a model ID like `"gemini-4-neon-high"` into `("gemini-4-neon", Some("high"))`.
pub fn split_model_and_level(model_id: &str) -> (String, Option<String>) {
    const SUFFIXES: &[&str] = &["-xhigh", "-high", "-medium", "-low", "-max"];
    let lower = model_id.to_lowercase();
    for suffix in SUFFIXES {
        if lower.ends_with(suffix) && model_id.len() > suffix.len() {
            let base = &model_id[..model_id.len() - suffix.len()];
            let lvl = suffix.trim_start_matches('-').to_string();
            return (base.to_string(), Some(lvl));
        }
    }
    (model_id.to_string(), None)
}

/// Group raw model IDs into `(base_model, vec![available_levels])` preserving discovery order.
pub fn group_models_by_base(raw_models: &[String]) -> Vec<(String, Vec<String>)> {
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for raw in raw_models {
        let (base, lvl) = split_model_and_level(raw);
        if let Some((_, levels)) = grouped.iter_mut().find(|(b, _)| b == &base) {
            if let Some(l) = lvl {
                if !levels.iter().any(|x| x == &l) {
                    levels.push(l);
                }
            }
        } else {
            let levels = match lvl {
                Some(l) => vec![l],
                None => Vec::new(),
            };
            grouped.push((base, levels));
        }
    }
    grouped
}

/// Recombine base model + selected level into the exact model ID expected by the CLI.
pub fn resolve_full_model_id(
    raw_models: &[String],
    base_model: &str,
    selected_level: Option<&str>,
) -> String {
    if let Some(lvl) = selected_level {
        let candidate = format!("{base_model}-{lvl}");
        if let Some(exact) = raw_models
            .iter()
            .find(|m| m.eq_ignore_ascii_case(&candidate))
        {
            return exact.clone();
        }
    }
    if let Some(exact) = raw_models
        .iter()
        .find(|m| m.eq_ignore_ascii_case(base_model))
    {
        return exact.clone();
    }
    base_model.to_string()
}

/// Dynamically fetch real available models from the active provider (no hardcoded/mock lists).
pub async fn fetch_models_for_provider(provider_id: &str) -> Vec<String> {
    match provider_id {
        "agy" => {
            if let Ok(output) = Command::new("agy")
                .arg("models")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .await
            {
                if output.status.success() {
                    return parse_cli_list_lines(&String::from_utf8_lossy(&output.stdout));
                }
            }
            Vec::new()
        }
        "ollama-http" | "ollama-cli" => {
            let host =
                env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
            let Ok(client) = reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(1200))
                .build()
            else {
                return Vec::new();
            };
            let url = format!("{}/api/tags", host.trim_end_matches('/'));
            if let Ok(resp) = client.get(&url).send().await {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = body["models"].as_array() {
                        return arr
                            .iter()
                            .filter_map(|m| m["name"].as_str().map(str::to_string))
                            .collect();
                    }
                }
            }
            Vec::new()
        }
        "openai" => {
            let Ok(key) = env::var("OPENAI_API_KEY") else {
                return Vec::new();
            };
            let Ok(client) = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
            else {
                return Vec::new();
            };
            if let Ok(resp) = client
                .get("https://api.openai.com/v1/models")
                .bearer_auth(key)
                .send()
                .await
            {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = body["data"].as_array() {
                        let mut ids: Vec<String> = arr
                            .iter()
                            .filter_map(|m| m["id"].as_str().map(str::to_string))
                            .collect();
                        ids.sort();
                        return ids;
                    }
                }
            }
            Vec::new()
        }
        "anthropic" => {
            let Ok(key) = env::var("ANTHROPIC_API_KEY") else {
                return Vec::new();
            };
            let Ok(client) = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
            else {
                return Vec::new();
            };
            if let Ok(resp) = client
                .get("https://api.anthropic.com/v1/models")
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
            {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = body["data"].as_array() {
                        return arr
                            .iter()
                            .filter_map(|m| m["id"].as_str().map(str::to_string))
                            .collect();
                    }
                }
            }
            Vec::new()
        }
        "gemini" | "google" => {
            let Ok(key) = env::var("GEMINI_API_KEY").or_else(|_| env::var("GOOGLE_API_KEY")) else {
                return Vec::new();
            };
            let Ok(client) = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
            else {
                return Vec::new();
            };
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models?key={key}"
            );
            if let Ok(resp) = client.get(&url).send().await {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = body["models"].as_array() {
                        return arr
                            .iter()
                            .filter_map(|m| {
                                m["name"]
                                    .as_str()
                                    .map(|s| s.trim_start_matches("models/").to_string())
                            })
                            .collect();
                    }
                }
            }
            Vec::new()
        }
        "openrouter" => {
            let Ok(key) = env::var("OPENROUTER_API_KEY") else {
                return Vec::new();
            };
            let Ok(client) = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
            else {
                return Vec::new();
            };
            if let Ok(resp) = client
                .get("https://openrouter.ai/api/v1/models")
                .bearer_auth(key)
                .send()
                .await
            {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = body["data"].as_array() {
                        return arr
                            .iter()
                            .filter_map(|m| m["id"].as_str().map(str::to_string))
                            .collect();
                    }
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Dynamically fetch real available agents from `agy agents` (no hardcoded/mock agents).
pub async fn fetch_agents_for_provider(provider_id: &str) -> Vec<String> {
    let mut list = vec!["Default".to_string()];
    if provider_id == "agy" {
        if let Ok(output) = Command::new("agy")
            .arg("agents")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
        {
            if output.status.success() {
                for a in parse_cli_list_lines(&String::from_utf8_lossy(&output.stdout)) {
                    if !list.iter().any(|x| x.eq_ignore_ascii_case(&a)) {
                        list.push(a);
                    }
                }
            }
        }
    }
    list
}

fn parse_cli_list_lines(raw: &str) -> Vec<String> {
    let mut items = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("Available")
            || trimmed.starts_with("Usage")
            || trimmed.starts_with("---")
            || trimmed.starts_with("===")
            || trimmed.starts_with("NAME")
            || trimmed.starts_with("MODEL")
            || trimmed.starts_with("ID")
        {
            continue;
        }
        // Strip leading bullets / markers like `* `, `- `, `• `, `✓ `
        let clean = trimmed
            .trim_start_matches(|c: char| matches!(c, '*' | '-' | '•' | '✓' | '▸' | ' '))
            .trim();
        // If tabular (2+ spaces or tab), take the first column (display name or model id)
        let first_col = clean
            .split("  ")
            .next()
            .unwrap_or(clean)
            .split('\t')
            .next()
            .unwrap_or(clean)
            .trim();
        if !first_col.is_empty() && !items.iter().any(|x| x == first_col) {
            items.push(first_col.to_string());
        }
    }
    items
}

/// Detect all AI providers available on the current machine (API keys, CLI tools, local Ollama).
pub async fn detect_providers() -> Vec<AiProvider> {
    let mut providers = Vec::new();

    // 1. Check API key environment variables
    let env_checks = [
        ("anthropic", "Anthropic Claude API", "ANTHROPIC_API_KEY"),
        ("openai", "OpenAI API", "OPENAI_API_KEY"),
        ("gemini", "Google Gemini API", "GEMINI_API_KEY"),
        ("google", "Google AI API", "GOOGLE_API_KEY"),
        ("openrouter", "OpenRouter API", "OPENROUTER_API_KEY"),
    ];

    for (id, name, var) in env_checks {
        let is_set = env::var(var).map(|v| !v.trim().is_empty()).unwrap_or(false);
        providers.push(AiProvider {
            id: id.to_string(),
            name: name.to_string(),
            kind: ProviderKind::ApiKey,
            detail: if is_set {
                format!("env `{var}` configured")
            } else {
                format!("env `{var}` not set")
            },
            available: is_set,
        });
    }

    // 2. Check installed AI CLIs in PATH
    let cli_checks = [
        ("agy", "Antigravity CLI (agy)", "agy"),
        ("claude-cli", "Claude Code CLI", "claude"),
        ("codex-cli", "OpenAI Codex CLI", "codex"),
        ("gemini-cli", "Gemini CLI", "gemini"),
        ("ollama-cli", "Ollama CLI", "ollama"),
        ("opencode", "OpenCode CLI", "opencode"),
        ("aider", "Aider CLI", "aider"),
        ("llm", "SimonW LLM CLI", "llm"),
    ];

    for (id, name, bin) in cli_checks {
        let found_path = which_in_path(bin);
        providers.push(AiProvider {
            id: id.to_string(),
            name: name.to_string(),
            kind: ProviderKind::LocalCli,
            detail: match &found_path {
                Some(p) => p.clone(),
                None => format!("`{bin}` not found in PATH"),
            },
            available: found_path.is_some(),
        });
    }

    // 3. Check local Ollama HTTP server
    let ollama_host =
        env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let ollama_running = check_ollama_server(&ollama_host).await;
    providers.push(AiProvider {
        id: "ollama-http".to_string(),
        name: "Ollama Local Server".to_string(),
        kind: ProviderKind::LocalService,
        detail: if let Some(model) = &ollama_running {
            format!("{ollama_host} (model: {model})")
        } else {
            format!("{ollama_host} (offline)")
        },
        available: ollama_running.is_some(),
    });

    providers
}

fn which_in_path(binary: &str) -> Option<String> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }
    None
}

async fn check_ollama_server(base_url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(700))
        .build()
        .ok()?;
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let first_model = body["models"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|m| m["name"].as_str())
        .unwrap_or("llama3")
        .to_string();
    Some(first_model)
}

/// Pick the first available provider, or a specific one by id.
pub fn select_provider<'a>(
    providers: &'a [AiProvider],
    preferred_id: Option<&str>,
) -> Option<&'a AiProvider> {
    if let Some(id) = preferred_id {
        if let Some(found) = providers.iter().find(|p| p.id == id && p.available) {
            return Some(found);
        }
    }
    providers.iter().find(|p| p.available)
}

/// Send a prompt to the chosen AI provider with default options.
pub async fn ask_provider(provider: &AiProvider, prompt: &str) -> Result<String> {
    ask_provider_with_options(provider, prompt, &AiRequestOptions::default()).await
}

/// Send a prompt to the chosen AI provider with explicit model, agent mode, and effort level.
pub async fn ask_provider_with_options(
    provider: &AiProvider,
    prompt: &str,
    opts: &AiRequestOptions,
) -> Result<String> {
    let model_override = opts
        .model
        .as_deref()
        .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("auto"));

    let agent_override = opts
        .agent
        .as_deref()
        .filter(|a| !a.is_empty() && !a.eq_ignore_ascii_case("default"));

    let sys_inst = opts
        .system_instruction
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let effective_prompt = match sys_inst {
        Some(inst) => format!("[System Instruction: {inst}]\n\n{prompt}"),
        None => prompt.to_string(),
    };

    let enriched_prompt = if let Some(agent_name) = agent_override {
        format!(
            "[Agent: {} | Effort: {}]\n\n{}",
            agent_name,
            opts.effort.label(),
            effective_prompt
        )
    } else {
        effective_prompt.clone()
    };

    let cli_prompt_with_files = if opts.attachments.is_empty() {
        effective_prompt.clone()
    } else {
        let list = opts
            .attachments
            .iter()
            .map(|p| format!("- {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{effective_prompt}\n\n[Attached images — please read/inspect these files:\n{list}]")
    };

    match provider.id.as_str() {
        "openai" => {
            ask_openai(&enriched_prompt, model_override, opts.effort, &opts.attachments).await
        }
        "anthropic" => {
            ask_anthropic(&enriched_prompt, model_override, opts.effort, &opts.attachments).await
        }
        "gemini" | "google" => ask_gemini(&enriched_prompt, model_override, &opts.attachments).await,
        "openrouter" => ask_openrouter(&enriched_prompt, model_override, &opts.attachments).await,
        "ollama-http" => ask_ollama_http(&enriched_prompt, model_override).await,
        "claude-cli" => {
            if let Some(m) = model_override {
                run_ai_cli("claude", &["--model", m, "-p", &cli_prompt_with_files]).await
            } else {
                run_ai_cli("claude", &["-p", &cli_prompt_with_files]).await
            }
        }
        "gemini-cli" => {
            if let Some(m) = model_override {
                run_ai_cli("gemini", &["-m", m, "-p", &cli_prompt_with_files]).await
            } else {
                run_ai_cli("gemini", &["-p", &cli_prompt_with_files]).await
            }
        }
        "codex-cli" => run_ai_cli("codex", &["exec", &cli_prompt_with_files]).await,
        "ollama-cli" => {
            let m = model_override.unwrap_or("llama3");
            run_ai_cli("ollama", &["run", m, &cli_prompt_with_files]).await
        }
        "llm" => run_ai_cli("llm", &[&cli_prompt_with_files]).await,
        "agy" => {
            let mut args: Vec<String> = Vec::new();
            let model_has_level = model_override
                .map(|m| split_model_and_level(m).1.is_some())
                .unwrap_or(false);
            if let Some(m) = model_override {
                args.push("--model".into());
                args.push(m.into());
            }
            if let Some(a) = agent_override {
                args.push("--agent".into());
                args.push(a.into());
            }
            if !model_has_level {
                args.push("--effort".into());
                args.push(opts.effort.api_value().into());
            }
            if let Some(mode) = opts.mode.as_deref().filter(|m| !m.is_empty()) {
                args.push("--mode".into());
                args.push(mode.into());
            }
            if opts.timeout_secs > 0 {
                args.push("--print-timeout".into());
                args.push(format!("{}s", opts.timeout_secs));
            }
            for img in &opts.attachments {
                if let Some(parent) = img.parent() {
                    args.push("--add-dir".into());
                    args.push(parent.display().to_string());
                }
            }
            args.push("-p".into());
            args.push(cli_prompt_with_files);
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run_ai_cli("agy", &arg_refs).await
        }
        other => Err(anyhow!(
            "Provider `{other}` detected, but automated non-interactive invocation is not configured yet."
        )),
    }
}

async fn ask_openai(
    prompt: &str,
    model_override: Option<&str>,
    effort: EffortLevel,
    attachments: &[PathBuf],
) -> Result<String> {
    let key = env::var("OPENAI_API_KEY").context("Missing OPENAI_API_KEY")?;
    let model = model_override
        .map(str::to_string)
        .or_else(|| env::var("OPENAI_MODEL").ok())
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    let images = load_encoded_images(attachments);
    let content = if images.is_empty() {
        json!(prompt)
    } else {
        let mut parts = vec![json!({"type": "text", "text": prompt})];
        for (mime, b64) in images {
            parts.push(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mime};base64,{b64}")}
            }));
        }
        json!(parts)
    };
    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": content}]
    });
    if model.starts_with("o1") || model.starts_with("o3") {
        body["reasoning_effort"] = json!(effort.api_value());
    }
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(key)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("(empty response)")
        .to_string())
}

async fn ask_openrouter(
    prompt: &str,
    model_override: Option<&str>,
    attachments: &[PathBuf],
) -> Result<String> {
    let key = env::var("OPENROUTER_API_KEY").context("Missing OPENROUTER_API_KEY")?;
    let model = model_override
        .map(str::to_string)
        .or_else(|| env::var("OPENROUTER_MODEL").ok())
        .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
    let images = load_encoded_images(attachments);
    let content = if images.is_empty() {
        json!(prompt)
    } else {
        let mut parts = vec![json!({"type": "text", "text": prompt})];
        for (mime, b64) in images {
            parts.push(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mime};base64,{b64}")}
            }));
        }
        json!(parts)
    };
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post("https://openrouter.ai/api/v1/chat/completions")
        .bearer_auth(key)
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": content}]
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("(empty response)")
        .to_string())
}

async fn ask_anthropic(
    prompt: &str,
    model_override: Option<&str>,
    effort: EffortLevel,
    attachments: &[PathBuf],
) -> Result<String> {
    let key = env::var("ANTHROPIC_API_KEY").context("Missing ANTHROPIC_API_KEY")?;
    let model = model_override
        .map(str::to_string)
        .or_else(|| env::var("ANTHROPIC_MODEL").ok())
        .unwrap_or_else(|| "claude-3-5-sonnet-latest".to_string());
    let max_tokens = match effort {
        EffortLevel::Low => 1024,
        EffortLevel::Medium => 2048,
        EffortLevel::High => 4096,
        EffortLevel::XHigh | EffortLevel::Max => 8192,
    };
    let images = load_encoded_images(attachments);
    let content = if images.is_empty() {
        json!(prompt)
    } else {
        let mut parts = Vec::new();
        for (mime, b64) in images {
            parts.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime,
                    "data": b64
                }
            }));
        }
        parts.push(json!({"type": "text", "text": prompt}));
        json!(parts)
    };
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": content}]
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(resp["content"][0]["text"]
        .as_str()
        .unwrap_or("(empty response)")
        .to_string())
}

async fn ask_gemini(
    prompt: &str,
    model_override: Option<&str>,
    attachments: &[PathBuf],
) -> Result<String> {
    let key = env::var("GEMINI_API_KEY")
        .or_else(|_| env::var("GOOGLE_API_KEY"))
        .context("Missing GEMINI_API_KEY or GOOGLE_API_KEY")?;
    let model = model_override
        .map(str::to_string)
        .or_else(|| env::var("GEMINI_MODEL").ok())
        .unwrap_or_else(|| "gemini-2.5-flash".to_string());
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={key}"
    );
    let mut parts = Vec::new();
    for (mime, b64) in load_encoded_images(attachments) {
        parts.push(json!({
            "inlineData": {"mimeType": mime, "data": b64}
        }));
    }
    parts.push(json!({"text": prompt}));
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(&url)
        .json(&json!({
            "contents": [{"parts": parts}]
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(resp["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("(empty response)")
        .to_string())
}

async fn ask_ollama_http(prompt: &str, model_override: Option<&str>) -> Result<String> {
    let host = env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let model = model_override
        .map(str::to_string)
        .or_else(|| env::var("OLLAMA_MODEL").ok())
        .unwrap_or_else(|| "llama3".to_string());
    let url = format!("{}/api/generate", host.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(&url)
        .json(&json!({
            "model": model,
            "prompt": prompt,
            "stream": false
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(resp["response"]
        .as_str()
        .unwrap_or("(empty response)")
        .to_string())
}

async fn run_ai_cli(bin: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("Failed to execute `{bin}`"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(anyhow!(
            "`{bin}` exited with {}: {}",
            output.status,
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}
