use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    // Appearance & Editor
    pub dark_mode: bool,
    pub font_family: String, // "Default" or installed system font name
    pub code_font_size: f32,
    pub diff_font_size: f32,
    pub ai_font_size: f32,
    pub terminal_font_size: f32,
    pub auto_refresh_diff: bool,
    pub auto_refresh_secs: u64,
    pub word_wrap_diff: bool,

    // AI & Agent
    pub execution_mode: String, // "auto" | "accept-edits" | "plan"
    pub system_instruction: String,
    pub request_timeout_secs: u64,
    pub selected_provider_id: Option<String>,
    pub selected_base_model: String,
    pub selected_level: Option<String>,
    pub selected_agent: String,

    // Workspace Layout
    pub show_diff_preview: bool,
    pub show_ai_panel: bool,
    pub show_terminal_panel: bool,

    // Terminal
    pub shell: String, // "zsh" | "bash" | "sh"
    pub max_terminal_lines: usize,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            dark_mode: true,
            font_family: "Default".to_string(),
            code_font_size: 12.5,
            diff_font_size: 12.5,
            ai_font_size: 12.5,
            terminal_font_size: 12.5,
            auto_refresh_diff: true,
            auto_refresh_secs: 2,
            word_wrap_diff: true,
            execution_mode: "auto".to_string(),
            system_instruction: String::new(),
            request_timeout_secs: 0,
            selected_provider_id: None,
            selected_base_model: String::new(),
            selected_level: None,
            selected_agent: "Default".to_string(),
            show_diff_preview: true,
            show_ai_panel: true,
            show_terminal_panel: true,
            shell: "zsh".to_string(),
            max_terminal_lines: 500,
        }
    }
}

impl AppSettings {
    pub fn config_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("cli-arai")
                .join("settings.json"),
        )
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = Self::config_path()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine HOME directory for settings"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(path)
    }
}
