mod ai;
mod changes;
mod config_rune;
mod desktop;
mod pty_terminal;
mod rune_engine;
mod settings;
mod tui;
mod ui_components;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "cli-arai",
    version,
    about = "cli arai — AI-powered Desktop App & CLI with Rust + Rune scripting, Diff UI, and split terminal"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch native Desktop App window (AI Prompt + Changes/Diff UI + Separate Terminal)
    Desktop,
    /// Launch interactive Terminal UI (TUI) inside the current terminal
    Tui,
    /// Manage and query locally detected AI providers
    Ai {
        #[command(subcommand)]
        action: AiCommands,
    },
    /// Run a Rune (.rn) script file (supports async/await and custom `cli::*` module)
    Run {
        /// Path to the .rn script file
        file: PathBuf,
    },
    /// Evaluate inline Rune code
    Eval {
        /// Rune code snippet
        code: String,
    },
    /// Start an interactive Rune REPL
    Repl,
}

#[derive(Subcommand)]
enum AiCommands {
    /// Detect all AI CLIs, API keys, and local services available on this machine
    List,
    /// Send a prompt directly to an available AI provider
    Ask {
        /// Prompt text to send
        prompt: String,
        /// Optional provider id (e.g. openai, anthropic, gemini, agy, claude-cli, ollama-http)
        #[arg(short, long)]
        provider: Option<String>,
    },
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let tracker = changes::ChangeTracker::new();
    rune_engine::init_tracker(tracker.clone());

    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Desktop) {
        Commands::Desktop => {
            // Must run on the main thread for macOS AppKit / native desktop window
            desktop::run_desktop(rt.handle().clone(), tracker)?;
        }
        Commands::Tui => {
            rt.block_on(tui::run_tui(tracker))?;
        }
        Commands::Ai { action } => match action {
            AiCommands::List => rt.block_on(async {
                let providers: Vec<_> = ai::detect_providers()
                    .await
                    .into_iter()
                    .filter(|p| p.available)
                    .collect();
                println!("=== Available AI Providers on This Machine ===");
                if providers.is_empty() {
                    println!("ไม่พบ AI ที่พร้อมใช้งาน");
                } else {
                    for p in &providers {
                        println!("✅ {:<12}  {:<24}  {}", p.id, p.name, p.detail);
                    }
                }
            }),
            AiCommands::Ask { prompt, provider } => rt.block_on(async {
                let providers = ai::detect_providers().await;
                let Some(selected) = ai::select_provider(&providers, provider.as_deref()) else {
                    eprintln!("ไม่พบ AI provider ที่พร้อมใช้งาน (ลองรัน `my-cli ai list` เพื่อตรวจสอบ)");
                    std::process::exit(1);
                };
                println!("[Using {} ({})]", selected.name, selected.id);
                let reply = ai::ask_provider(selected, &prompt).await?;
                println!("{reply}");
                Ok::<(), anyhow::Error>(())
            })?,
        },
        Commands::Run { file } => {
            let result = rt.block_on(rune_engine::run_file(&file))?;
            println!("{result}");
        }
        Commands::Eval { code } => {
            let result = rt.block_on(rune_engine::eval_inline(&code))?;
            println!("{result}");
        }
        Commands::Repl => {
            rt.block_on(run_repl())?;
        }
    }

    Ok(())
}

async fn run_repl() -> Result<()> {
    println!("cli arai — Rune REPL (async/await + custom `cli::*` module)");
    println!("พิมพ์โค้ด Rune แล้วกด Enter | พิมพ์ `exit` หรือ `quit` เพื่อออก\n");

    let stdin = io::stdin();
    let mut line = String::new();

    loop {
        print!("rune> ");
        io::stdout().flush()?;
        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "exit" | "quit" | ":q") {
            break;
        }
        match rune_engine::eval_inline(trimmed).await {
            Ok(out) => println!("=> {out}"),
            Err(e) => eprintln!("Error: {e}"),
        }
    }

    Ok(())
}
