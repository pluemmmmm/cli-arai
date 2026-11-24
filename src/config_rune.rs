use anyhow::{anyhow, Result};
use rune::termcolor::Buffer;
use rune::{Context, Diagnostics, FromValue, Source, Sources, Vm};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, FromValue)]
pub struct LiveConfig {
    pub accent_hex: String,
    pub rune_accent_hex: String,
    pub bg_canvas_hex: String,
    pub bg_panel_hex: String,
    pub code_font_size: f32,
    pub terminal_commands: Vec<String>,
    pub prompt_presets: Vec<String>,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            accent_hex: "#3B82F6".into(),
            rune_accent_hex: "#C084FC".into(),
            bg_canvas_hex: "#181818".into(),
            bg_panel_hex: "#1F1F1F".into(),
            code_font_size: 12.5,
            terminal_commands: vec![
                "git status".into(),
                "ls -la".into(),
                "cargo check".into(),
            ],
            prompt_presets: vec![
                "Detect AI|/ai".into(),
                "▶ hello.rn|/run examples/hello.rn".into(),
            ],
        }
    }
}

/// Evaluate `config.rn` using the Rune VM and deserialize the returned object into `LiveConfig`.
pub fn load_from_file(path: &Path) -> Result<LiveConfig> {
    let code = std::fs::read_to_string(path)?;
    let context = Context::with_default_modules()?;
    let runtime = Arc::new(context.runtime()?);

    let mut sources = Sources::new();
    sources.insert(Source::new(path.display().to_string(), code)?)?;

    let mut diagnostics = Diagnostics::new();
    let result = rune::prepare(&mut sources)
        .with_context(&context)
        .with_diagnostics(&mut diagnostics)
        .build();

    if !diagnostics.is_empty() && result.is_err() {
        let mut buf = Buffer::no_color();
        diagnostics.emit(&mut buf, &sources)?;
        return Err(anyhow!(
            "config.rn syntax error:\n{}",
            String::from_utf8_lossy(buf.as_slice())
        ));
    }

    let unit = result?;
    let mut vm = Vm::new(runtime, Arc::new(unit));
    let val = vm.call(["config"], ())?;
    let cfg: LiveConfig = rune::from_value(val)?;
    Ok(cfg)
}
