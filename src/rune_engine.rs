use crate::ai;
use crate::changes::ChangeTracker;
use anyhow::{anyhow, Result};
use rune::termcolor::Buffer;
use rune::{Context, ContextError, Diagnostics, Module, Source, Sources, Value, Vm};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tokio::process::Command;

static GLOBAL_TRACKER: OnceLock<ChangeTracker> = OnceLock::new();

pub fn init_tracker(tracker: ChangeTracker) {
    let _ = GLOBAL_TRACKER.set(tracker);
}

// --- Custom Rust Functions exposed to Rune scripts (`cli::*`) ---

#[rune::function]
fn read_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("read_file error: {e}"))
}

#[rune::function]
fn write_file(path: &str, content: &str) -> Result<String, String> {
    let old = std::fs::read_to_string(path).ok();
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
    }
    std::fs::write(path, content).map_err(|e| format!("write_file error: {e}"))?;

    if let Some(tracker) = GLOBAL_TRACKER.get() {
        tracker.record_edit(path, old.as_deref(), content);
    }

    Ok(format!("Wrote {} bytes to {path}", content.len()))
}

#[rune::function]
fn list_dir(path: &str) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| format!("list_dir error: {e}"))? {
        let entry = entry.map_err(|e| format!("entry error: {e}"))?;
        entries.push(entry.file_name().to_string_lossy().to_string());
    }
    entries.sort();
    Ok(entries)
}

#[rune::function]
async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

#[rune::function]
async fn run_cmd(cmd: String) -> Result<String, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .await
        .map_err(|e| format!("run_cmd error: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        Ok(stdout.trim().to_string())
    } else {
        Err(format!(
            "exit {}: {}{}",
            output.status,
            stdout.trim(),
            stderr.trim()
        ))
    }
}

#[rune::function]
async fn detect_ai() -> Vec<String> {
    ai::detect_providers()
        .await
        .into_iter()
        .filter(|p| p.available)
        .map(|p| format!("{} ({})", p.id, p.name))
        .collect()
}

#[rune::function]
async fn ask_ai(prompt: String) -> Result<String, String> {
    let providers = ai::detect_providers().await;
    let Some(provider) = ai::select_provider(&providers, None) else {
        return Err("No AI provider available on this machine.".to_string());
    };
    ai::ask_provider(provider, &prompt)
        .await
        .map_err(|e| e.to_string())
}

/// Build the custom `cli` Rune module.
pub fn cli_module() -> Result<Module, ContextError> {
    let mut module = Module::with_crate("cli")?;
    module.function_meta(read_file)?;
    module.function_meta(write_file)?;
    module.function_meta(list_dir)?;
    module.function_meta(sleep_ms)?;
    module.function_meta(run_cmd)?;
    module.function_meta(detect_ai)?;
    module.function_meta(ask_ai)?;
    Ok(module)
}

/// Build a Rune Context containing default modules + our custom `cli` module.
pub fn build_context() -> Result<Context> {
    let mut context = Context::with_default_modules()?;
    context.install(cli_module()?)?;
    Ok(context)
}

/// Compile and execute a Rune source string asynchronously (supports both `pub fn main()` and `pub async fn main()`).
///
/// Runs the `!Send` Rune `Vm` on a blocking thread via `Handle::block_on` so the returned future is `Send`
/// and can be used inside `tokio::spawn`.
pub async fn eval_script(name: &str, code: &str) -> Result<String> {
    let name = name.to_string();
    let code = code.to_string();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(eval_script_local(&name, &code)))
        .await
        .map_err(|e| anyhow!("Rune task join error: {e}"))?
}

async fn eval_script_local(name: &str, code: &str) -> Result<String> {
    let context = build_context()?;
    let runtime = Arc::new(context.runtime()?);

    let mut sources = Sources::new();
    sources.insert(Source::new(name, code)?)?;

    let mut diagnostics = Diagnostics::new();
    let result = rune::prepare(&mut sources)
        .with_context(&context)
        .with_diagnostics(&mut diagnostics)
        .build();

    if !diagnostics.is_empty() {
        let mut buffer = Buffer::no_color();
        diagnostics.emit(&mut buffer, &sources)?;
        let diag_text = String::from_utf8_lossy(buffer.as_slice()).to_string();
        if result.is_err() {
            return Err(anyhow!("Rune compile error:\n{diag_text}"));
        }
    }

    let unit = result?;
    let mut vm = Vm::new(runtime, Arc::new(unit));

    let value: Value = vm.async_call(["main"], ()).await?;
    Ok(format!("{value:?}"))
}

/// Evaluate a Rune expression or snippet by wrapping it in `pub async fn main()` if needed.
pub async fn eval_inline(snippet: &str) -> Result<String> {
    let trimmed = snippet.trim();
    if trimmed.contains("fn main") {
        eval_script("<inline>", trimmed).await
    } else {
        let wrapped = format!("pub async fn main() {{\n{trimmed}\n}}");
        eval_script("<inline>", &wrapped).await
    }
}

/// Run a `.rn` Rune script file from disk.
pub async fn run_file(path: &Path) -> Result<String> {
    let code = std::fs::read_to_string(path)?;
    let name = path.display().to_string();
    eval_script(&name, &code).await
}
