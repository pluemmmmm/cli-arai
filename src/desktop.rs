use crate::ai::{self, AiProvider, AiRequestOptions, AiStreamEvent, EffortLevel};
use crate::changes::{self, ChangeTracker, CommitGraphRow, DiffLine, DiffTag, FileChange};
use crate::config_rune::{self, LiveConfig};
use crate::rune_engine;
use crate::settings::AppSettings;
use crate::ui_components::{self, ThemeColors};
use anyhow::Result;
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Frame, Margin, RichText,
    Stroke, TextEdit, Vec2,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TermLineKind {
    Command,
    Output,
    Error,
    Success,
}

impl TermLineKind {
    fn color(self, t: &ThemeColors) -> Color32 {
        match self {
            Self::Command => t.text_secondary,
            Self::Output => t.text_primary,
            Self::Error => t.accent_rose,
            Self::Success => t.accent_emerald,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffViewMode {
    Unified,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Changes,
    Graph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Ai,
    Terminal,
}

#[derive(Debug, Clone)]
struct ChatMessage {
    role: String,
    content: String,
    attachments: Vec<String>,
    meta: Option<String>,
    activities: Vec<(String, String)>,
}

impl ChatMessage {
    fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            attachments: Vec::new(),
            meta: None,
            activities: Vec::new(),
        }
    }

    fn error(content: impl Into<String>) -> Self {
        Self {
            role: "error".into(),
            content: content.into(),
            attachments: Vec::new(),
            meta: None,
            activities: Vec::new(),
        }
    }

    fn user(content: impl Into<String>, attachments: Vec<String>) -> Self {
        Self {
            role: "you".into(),
            content: content.into(),
            attachments,
            meta: None,
            activities: Vec::new(),
        }
    }

    fn ai(
        role: impl Into<String>,
        content: impl Into<String>,
        meta: Option<String>,
        activities: Vec<(String, String)>,
    ) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            attachments: Vec::new(),
            meta,
            activities,
        }
    }
}

struct TermTab {
    id: usize,
    name: String,
    cwd: PathBuf,
    git_branch: Option<String>,
    lines: Vec<(String, TermLineKind)>,
    input: String,
    history: Vec<String>,
    history_idx: Option<usize>,
    busy: bool,
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    pty: Option<crate::pty_terminal::PtyTerminal>,
    pty_error: Option<String>,
}

impl TermTab {
    fn new(id: usize, cwd: PathBuf, ctx: egui::Context) -> Self {
        let git_branch = detect_git_branch(&cwd);
        let (pty, pty_error) = match crate::pty_terminal::PtyTerminal::spawn(&cwd, ctx) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(format!("Failed to start PTY shell: {e}"))),
        };
        Self {
            id,
            name: format!("zsh {id}"),
            cwd,
            git_branch,
            lines: Vec::new(),
            input: String::new(),
            history: Vec::new(),
            history_idx: None,
            busy: false,
            cancel_tx: None,
            pty,
            pty_error,
        }
    }

    fn cancel_running(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        self.busy = false;
        self.lines.push(("^C".to_string(), TermLineKind::Error));
    }

    fn refresh_branch(&mut self) {
        self.git_branch = detect_git_branch(&self.cwd);
    }
}

/// Git Bash-style Tab Autocomplete for the integrated terminal.
/// Returns `(updated_input, optional_candidates_preview)`.
fn tab_complete_terminal(cwd: &std::path::Path, input: &str) -> (String, Option<String>) {
    // Split into (before_last_token, last_token)
    let (before, token) = match input.rfind(' ') {
        Some(pos) => (&input[..=pos], &input[pos + 1..]),
        None => ("", input),
    };

    let trimmed_before = before.trim();
    let mut matches: Vec<String> = Vec::new();

    // 1. Git branch completion (`git checkout <tab>`, `git switch <tab>`, `git merge <tab>`)
    if matches!(
        trimmed_before,
        "git checkout" | "git switch" | "git merge" | "git rebase" | "git branch -d" | "git branch -D"
    ) {
        for b in list_git_branches(cwd) {
            if b.starts_with(token) {
                matches.push(b);
            }
        }
    }
    // 2. Git subcommand completion (`git <tab>`)
    else if trimmed_before == "git" {
        const GIT_SUBS: &[&str] = &[
            "add", "branch", "checkout", "clone", "commit", "diff", "fetch", "init", "log",
            "merge", "pull", "push", "rebase", "reset", "restore", "show", "stash", "status",
            "switch", "tag",
        ];
        for sub in GIT_SUBS {
            if sub.starts_with(token) {
                matches.push((*sub).to_string());
            }
        }
    }
    // 3. File / directory completion (and common commands when completing the first word)
    else {
        let (dir_prefix, partial) = match token.rfind('/') {
            Some(pos) => (&token[..=pos], &token[pos + 1..]),
            None => ("", token),
        };

        let search_dir = if dir_prefix.is_empty() {
            cwd.to_path_buf()
        } else if let Some(rest) = dir_prefix.strip_prefix("~/") {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(rest))
                .unwrap_or_else(|| cwd.join(dir_prefix))
        } else {
            cwd.join(dir_prefix)
        };

        if let Ok(entries) = std::fs::read_dir(&search_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip dotfiles unless user explicitly typed `.`
                if name.starts_with('.') && !partial.starts_with('.') {
                    continue;
                }
                if name.starts_with(partial) {
                    let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                    let suffix = if is_dir { "/" } else { "" };
                    matches.push(format!("{dir_prefix}{name}{suffix}"));
                }
            }
        }

        // If completing the first word without `/`, also offer common shell/dev commands
        if before.is_empty() && dir_prefix.is_empty() {
            const CMDS: &[&str] = &[
                "agy", "brew", "bun", "cargo", "cat", "cd", "clear", "code", "cp", "echo", "find",
                "git", "grep", "ls", "mkdir", "mv", "node", "npm", "open", "pnpm", "pwd",
                "python3", "rm", "rustc", "touch", "watchexec",
            ];
            for cmd in CMDS {
                if cmd.starts_with(partial) && !matches.iter().any(|m| m == cmd) {
                    matches.push((*cmd).to_string());
                }
            }
        }
    }

    matches.sort();
    matches.dedup();

    if matches.is_empty() {
        return (input.to_string(), None);
    }

    if matches.len() == 1 {
        let m = &matches[0];
        let trailing = if m.ends_with('/') { "" } else { " " };
        return (format!("{before}{m}{trailing}"), None);
    }

    // Multiple matches: complete longest common prefix + display candidates like Git Bash
    let mut lcp = matches[0].clone();
    for m in &matches[1..] {
        let common_len = lcp
            .chars()
            .zip(m.chars())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum();
        lcp.truncate(common_len);
    }

    let updated = format!("{before}{lcp}");
    let preview = matches.join("   ");
    (updated, Some(preview))
}

/// Detect the real local machine hostname (short name via `hostname -s`).
fn detect_system_hostname() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

/// Format a path like Git Bash (`~/Desktop/my-cli`).
fn format_git_bash_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home_path = std::path::Path::new(&home);
        if let Ok(rel) = path.strip_prefix(home_path) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

/// Fast zero-subprocess git branch detector (walks up for `.git/HEAD`).
fn detect_git_branch(dir: &std::path::Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(p) = cur {
        let head_path = p.join(".git").join("HEAD");
        if let Ok(content) = std::fs::read_to_string(&head_path) {
            let trimmed = content.trim();
            if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
                return Some(branch.to_string());
            }
            if trimmed.len() >= 7 {
                return Some(trimmed[..7].to_string());
            }
        }
        cur = p.parent();
    }
    None
}

/// List all local git branches in the given directory.
fn list_git_branches(dir: &std::path::Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(["branch", "--format=%(refname:short)"])
        .current_dir(dir)
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

enum BgEvent {
    ProvidersLoaded(Vec<AiProvider>),
    ModelsAndAgentsLoaded {
        models: Vec<String>,
        agents: Vec<String>,
    },
    AiStream(AiStreamEvent),
    AiReply {
        provider: String,
        model: String,
        text: String,
        duration_ms: u128,
    },
    AiError(String),
    RuneResult(String),
    TerminalLine {
        tab_id: usize,
        line: String,
        is_err: bool,
    },
    TerminalDone {
        tab_id: usize,
    },
    ConfigReloaded(LiveConfig),
    ConfigError(String),
}

/// Strip ANSI CSI escape sequences (e.g. `\x1b[32m`) so dev-server output reads cleanly.
fn strip_ansi_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

fn parse_hex_color(hex: &str, fallback: Color32) -> Color32 {
    let s = hex.trim().trim_start_matches('#');
    if s.len() != 6 {
        return fallback;
    }
    let Ok(r) = u8::from_str_radix(&s[0..2], 16) else {
        return fallback;
    };
    let Ok(g) = u8::from_str_radix(&s[2..4], 16) else {
        return fallback;
    };
    let Ok(b) = u8::from_str_radix(&s[4..6], 16) else {
        return fallback;
    };
    Color32::from_rgb(r, g, b)
}

/// Ensure only a single desktop instance runs at a time by closing any previous `cli-arai` instance.
fn replace_existing_instance() {
    let pid_path = std::env::temp_dir().join("cli-arai-desktop.pid");
    let current_pid = std::process::id();

    #[cfg(unix)]
    if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(old_pid) = content.trim().parse::<u32>() {
            if old_pid != current_pid {
                let is_same_app = std::process::Command::new("ps")
                    .args(["-p", &old_pid.to_string(), "-o", "comm="])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().ends_with("cli-arai"))
                    .unwrap_or(false);

                if is_same_app {
                    let _ = std::process::Command::new("kill")
                        .args(["-TERM", &old_pid.to_string()])
                        .status();
                }
            }
        }
    }

    let _ = std::fs::write(&pid_path, current_pid.to_string());
}

pub fn run_desktop(rt: Handle, tracker: ChangeTracker) -> Result<()> {
    replace_existing_instance();
    let cwd = std::env::current_dir()?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("cli arai")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1024.0, 660.0])
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };

    eframe::run_native(
        "cli arai",
        options,
        Box::new(move |cc| {
            let loaded_settings = AppSettings::load();
            let system_fonts = discover_system_fonts();
            setup_fonts(&cc.egui_ctx, &loaded_settings.font_family, &system_fonts);
            ui_components::apply_theme(&cc.egui_ctx, loaded_settings.dark_mode);
            Ok(Box::new(DesktopApp::new(
                rt,
                cwd,
                tracker,
                cc.egui_ctx.clone(),
                loaded_settings,
                system_fonts,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Desktop window error: {e}"))
}

fn discover_system_fonts() -> Vec<(String, PathBuf)> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        dirs.push(h.join("Library/Fonts"));
        dirs.push(h.join(".local/share/fonts"));
    }
    dirs.push(PathBuf::from("/Library/Fonts"));
    dirs.push(PathBuf::from("/System/Library/Fonts"));
    dirs.push(PathBuf::from("/System/Library/Fonts/Supplemental"));
    dirs.push(PathBuf::from("/usr/share/fonts"));

    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();

    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ext != "ttf" && ext != "otf" {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if stem.starts_with('.') {
                    continue;
                }
                let display = stem.to_string();
                let key = display.to_ascii_lowercase();
                if seen.insert(key) {
                    found.push((display, path));
                }
            }
        }
    }

    found.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
    found
}

fn setup_fonts(
    ctx: &egui::Context,
    selected_font_name: &str,
    system_fonts: &[(String, PathBuf)],
) {
    let mut fonts = FontDefinitions::default();

    // 1. User-selected system font (placed first in both Proportional & Monospace)
    if !selected_font_name.is_empty() && selected_font_name != "Default" {
        if let Some((_, path)) = system_fonts
            .iter()
            .find(|(name, _)| name == selected_font_name)
        {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("user_font".to_owned(), Arc::new(FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(FontFamily::Proportional)
                    .or_default()
                    .insert(0, "user_font".to_owned());
                fonts
                    .families
                    .entry(FontFamily::Monospace)
                    .or_default()
                    .insert(0, "user_font".to_owned());
            }
        }
    }

    // 2. Thai fallback font so Thai text always renders cleanly
    let candidates = [
        "/System/Library/Fonts/Supplemental/Ayuthaya.ttf",
        "/System/Library/Fonts/Supplemental/Tahoma.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/tlwg/Garuda.ttf",
        "C:\\Windows\\Fonts\\tahoma.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("thai_font".to_owned(), Arc::new(FontData::from_owned(bytes)));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("thai_font".to_owned());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("thai_font".to_owned());
            break;
        }
    }

    ctx.set_fonts(fonts);
}

struct DesktopApp {
    rt: Handle,
    ctx: egui::Context,
    cwd: PathBuf,
    hostname: String,
    recent_projects: Vec<PathBuf>,
    current_branch: Option<String>,
    branches: Vec<String>,
    show_new_branch_modal: bool,
    new_branch_input: String,
    tracker: ChangeTracker,

    providers: Vec<AiProvider>,
    selected_provider_id: Option<String>,
    raw_models: Vec<String>,
    grouped_models: Vec<(String, Vec<String>)>,
    selected_base_model: String,
    selected_level: Option<String>,
    available_agents: Vec<String>,
    selected_agent: String,
    selected_effort: EffortLevel,

    // Orca-style Diff Workspace & Commit Graph
    sidebar_tab: SidebarTab,
    changes: Vec<FileChange>,
    selected_change_idx: usize,
    commit_graph: Vec<CommitGraphRow>,
    selected_commit_hash: Option<String>,
    selected_commit_changes: Vec<FileChange>,
    selected_commit_file_idx: usize,
    file_filter: String,
    diff_mode: DiffViewMode,
    last_auto_refresh: std::time::Instant,

    // Independent Panel Visibility Toggles: Diff Preview, AI Prompt, Terminal
    show_diff_preview: bool,
    show_ai_panel: bool,
    show_terminal_panel: bool,
    bottom_split_ratio: f32,
    chat: Vec<ChatMessage>,
    prompt_input: String,
    attached_images: Vec<PathBuf>,
    ai_busy: bool,
    ai_has_session: bool,
    ai_conversation_id: Option<String>,
    ai_status: String,
    ai_started_at: Option<std::time::Instant>,
    ai_live_thought: Option<String>,
    ai_live_activities: Vec<(bool, String, String)>,

    term_tabs: Vec<TermTab>,
    active_term_idx: usize,
    next_term_id: usize,

    // Persisted App Settings + Hot-reloadable Rune config (`config.rn`)
    settings: AppSettings,
    system_fonts: Vec<(String, PathBuf)>,
    show_settings_modal: bool,
    settings_tab: SettingsTab,
    settings_status: Option<String>,
    dark_mode: bool,
    live_cfg: LiveConfig,
    config_reloads: usize,
    config_error: Option<String>,

    tx: mpsc::UnboundedSender<BgEvent>,
    rx: mpsc::UnboundedReceiver<BgEvent>,
}

impl DesktopApp {
    fn persist_ui_state(&mut self) {
        self.settings.selected_provider_id = self.selected_provider_id.clone();
        self.settings.selected_base_model = self.selected_base_model.clone();
        self.settings.selected_level = self.selected_level.clone();
        self.settings.selected_agent = self.selected_agent.clone();
        self.settings.show_diff_preview = self.show_diff_preview;
        self.settings.show_ai_panel = self.show_ai_panel;
        self.settings.show_terminal_panel = self.show_terminal_panel;
        let _ = self.settings.save();
    }

    fn render_ai_and_terminal_area(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        t: &ThemeColors,
        accent: Color32,
    ) {
        let full_rect = ui.available_rect_before_wrap();
        // Reserve full_rect in parent ui so TopBottomPanel preserves its height & resizes smoothly
        ui.allocate_rect(full_rect, egui::Sense::hover());

        match (self.show_ai_panel, self.show_terminal_panel) {
            (true, true) => {
                let gap = 10.0;
                let total_w = (full_rect.width() - gap).max(300.0);
                let min_pane_w = 220.0f32.min(total_w * 0.35);

                let left_w = (total_w * self.bottom_split_ratio)
                    .clamp(min_pane_w, (total_w - min_pane_w).max(min_pane_w));
                let right_w = (total_w - left_w).max(min_pane_w);

                let left_rect = egui::Rect::from_min_size(
                    full_rect.min,
                    Vec2::new(left_w, full_rect.height()),
                );
                let handle_rect = egui::Rect::from_min_size(
                    egui::pos2(left_rect.max.x, full_rect.min.y),
                    Vec2::new(gap, full_rect.height()),
                );
                let right_rect = egui::Rect::from_min_size(
                    egui::pos2(handle_rect.max.x, full_rect.min.y),
                    Vec2::new(right_w, full_rect.height()),
                );

                // Left Pane: AI
                let mut left_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(left_rect)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                left_ui.set_clip_rect(left_rect);
                Frame::new()
                    .fill(t.bg_code)
                    .stroke(Stroke::new(1.0_f32, t.border_hairline))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(&mut left_ui, |ui| {
                        ui.set_min_size(Vec2::new(
                            (left_w - 24.0).max(80.0),
                            (full_rect.height() - 24.0).max(80.0),
                        ));
                        ui.set_max_width((left_w - 24.0).max(80.0));
                        self.render_bottom_prompt(ui);
                    });

                // Centered Draggable Split Handle between AI CHAT and TERMINAL
                let handle_resp = ui
                    .interact(
                        handle_rect,
                        ui.make_persistent_id("bottom_split_handle"),
                        egui::Sense::drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);

                if handle_resp.dragged() {
                    if let Some(pointer) = ctx.pointer_interact_pos() {
                        let new_left = (pointer.x - full_rect.min.x - gap * 0.5)
                            .clamp(min_pane_w, total_w - min_pane_w);
                        self.bottom_split_ratio = (new_left / total_w).clamp(0.15, 0.85);
                    }
                }

                let grip_color = if handle_resp.dragged() {
                    accent
                } else if handle_resp.hovered() {
                    t.text_secondary
                } else {
                    t.border_subtle
                };
                let grip_h = if handle_resp.hovered() || handle_resp.dragged() {
                    48.0
                } else {
                    28.0
                };
                let grip_rect =
                    egui::Rect::from_center_size(handle_rect.center(), Vec2::new(3.0, grip_h));
                ui.painter()
                    .rect_filled(grip_rect, CornerRadius::same(2), grip_color);

                // Right Pane: TERMINAL
                let mut right_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(right_rect)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                right_ui.set_clip_rect(right_rect);
                Frame::new()
                    .fill(t.bg_code)
                    .stroke(Stroke::new(1.0_f32, t.border_hairline))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(&mut right_ui, |ui| {
                        ui.set_min_size(Vec2::new(
                            (right_w - 24.0).max(80.0),
                            (full_rect.height() - 24.0).max(80.0),
                        ));
                        ui.set_max_width((right_w - 24.0).max(80.0));
                        self.render_bottom_terminal(ui);
                    });
            }
            (true, false) => {
                let mut ai_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(full_rect)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                ai_ui.set_clip_rect(full_rect);
                Frame::new()
                    .fill(t.bg_code)
                    .stroke(Stroke::new(1.0_f32, t.border_hairline))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(&mut ai_ui, |ui| {
                        ui.set_min_size(Vec2::new(
                            (full_rect.width() - 24.0).max(80.0),
                            (full_rect.height() - 24.0).max(80.0),
                        ));
                        ui.set_max_width((full_rect.width() - 24.0).max(80.0));
                        self.render_bottom_prompt(ui);
                    });
            }
            (false, true) => {
                let mut term_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(full_rect)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                term_ui.set_clip_rect(full_rect);
                Frame::new()
                    .fill(t.bg_code)
                    .stroke(Stroke::new(1.0_f32, t.border_hairline))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(&mut term_ui, |ui| {
                        ui.set_min_size(Vec2::new(
                            (full_rect.width() - 24.0).max(80.0),
                            (full_rect.height() - 24.0).max(80.0),
                        ));
                        ui.set_max_width((full_rect.width() - 24.0).max(80.0));
                        self.render_bottom_terminal(ui);
                    });
            }
            (false, false) => {}
        }
    }

    fn new(
        rt: Handle,
        cwd: PathBuf,
        tracker: ChangeTracker,
        ctx: egui::Context,
        settings: AppSettings,
        system_fonts: Vec<(String, PathBuf)>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let changes = tracker.snapshot(&cwd);
        let dark_mode = settings.dark_mode;

        let config_path = cwd.join("config.rn");
        let (live_cfg, config_error) = match config_rune::load_from_file(&config_path) {
            Ok(c) => (c, None),
            Err(_) => (LiveConfig::default(), None),
        };

        let current_branch = detect_git_branch(&cwd);
        let branches = list_git_branches(&cwd);

        let hostname = detect_system_hostname();
        let commit_graph = changes::collect_commit_graph(&cwd, 60);

        let app = Self {
            rt: rt.clone(),
            ctx: ctx.clone(),
            cwd: cwd.clone(),
            hostname,
            recent_projects: vec![cwd.clone()],
            current_branch,
            branches,
            show_new_branch_modal: false,
            new_branch_input: String::new(),
            tracker,
            providers: Vec::new(),
            selected_provider_id: settings.selected_provider_id.clone(),
            raw_models: Vec::new(),
            grouped_models: Vec::new(),
            selected_base_model: settings.selected_base_model.clone(),
            selected_level: settings.selected_level.clone(),
            available_agents: vec!["Default".to_string()],
            selected_agent: if settings.selected_agent.is_empty() {
                "Default".to_string()
            } else {
                settings.selected_agent.clone()
            },
            selected_effort: EffortLevel::Medium,
            sidebar_tab: SidebarTab::Changes,
            changes,
            selected_change_idx: 0,
            commit_graph,
            selected_commit_hash: None,
            selected_commit_changes: Vec::new(),
            selected_commit_file_idx: 0,
            file_filter: String::new(),
            diff_mode: DiffViewMode::Unified,
            last_auto_refresh: std::time::Instant::now(),
            show_diff_preview: settings.show_diff_preview,
            show_ai_panel: settings.show_ai_panel,
            show_terminal_panel: settings.show_terminal_panel,
            bottom_split_ratio: 0.5,
            chat: Vec::new(),
            prompt_input: String::new(),
            attached_images: Vec::new(),
            ai_busy: false,
            ai_has_session: false,
            ai_conversation_id: None,
            ai_status: String::new(),
            ai_started_at: None,
            ai_live_thought: None,
            ai_live_activities: Vec::new(),
            term_tabs: vec![TermTab::new(1, cwd.clone(), ctx.clone())],
            active_term_idx: 0,
            next_term_id: 2,
            settings,
            system_fonts,
            show_settings_modal: false,
            settings_tab: SettingsTab::General,
            settings_status: None,
            dark_mode,
            live_cfg,
            config_reloads: 0,
            config_error,
            tx: tx.clone(),
            rx,
        };

        // Background AI provider detection
        let tx_ai = tx.clone();
        let ctx_ai = ctx.clone();
        rt.spawn(async move {
            let list = ai::detect_providers().await;
            let _ = tx_ai.send(BgEvent::ProvidersLoaded(list));
            ctx_ai.request_repaint();
        });

        // Background Hot-Reload watcher for `config.rn`
        let tx_cfg = tx;
        let ctx_cfg = ctx;
        rt.spawn(async move {
            let mut last_mod = std::fs::metadata(&config_path)
                .and_then(|m| m.modified())
                .ok();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                let cur_mod = std::fs::metadata(&config_path)
                    .and_then(|m| m.modified())
                    .ok();
                if cur_mod.is_some() && cur_mod != last_mod {
                    last_mod = cur_mod;
                    match config_rune::load_from_file(&config_path) {
                        Ok(cfg) => {
                            let _ = tx_cfg.send(BgEvent::ConfigReloaded(cfg));
                        }
                        Err(err) => {
                            let _ = tx_cfg.send(BgEvent::ConfigError(err.to_string()));
                        }
                    }
                    ctx_cfg.request_repaint();
                }
            }
        });

        app
    }

    fn active_provider(&self) -> Option<&AiProvider> {
        ai::select_provider(&self.providers, self.selected_provider_id.as_deref())
    }

    fn refresh_changes(&mut self) {
        self.changes = self.tracker.snapshot(&self.cwd);
        if self.selected_change_idx >= self.changes.len() && !self.changes.is_empty() {
            self.selected_change_idx = self.changes.len() - 1;
        }
        self.commit_graph = changes::collect_commit_graph(&self.cwd, 60);
        self.refresh_git_branches();
    }

    fn select_commit(&mut self, hash: String) {
        self.selected_commit_changes = changes::collect_commit_diff(&self.cwd, &hash);
        self.selected_commit_file_idx = 0;
        self.selected_commit_hash = Some(hash);
    }

    fn refresh_git_branches(&mut self) {
        self.current_branch = detect_git_branch(&self.cwd);
        self.branches = list_git_branches(&self.cwd);
        for tab in &mut self.term_tabs {
            tab.refresh_branch();
        }
    }

    fn checkout_branch(&mut self, branch: &str, create_new: bool) {
        let branch = branch.trim();
        if branch.is_empty() {
            return;
        }
        let args: Vec<&str> = if create_new {
            vec!["checkout", "-b", branch]
        } else {
            vec!["checkout", branch]
        };

        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(&self.cwd)
            .output();

        match out {
            Ok(o) if o.status.success() => {
                self.refresh_changes();
                let msg = if create_new {
                    format!("สร้างและสลับไปที่ branch ใหม่: {branch}")
                } else {
                    format!("สลับไปที่ branch: {branch}")
                };
                if let Some(tab) = self.term_tabs.get_mut(self.active_term_idx) {
                    tab.lines.push((msg.clone(), TermLineKind::Success));
                }
                self.chat.push(ChatMessage::system(msg));
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                let msg = format!("git checkout error: {err}");
                if let Some(tab) = self.term_tabs.get_mut(self.active_term_idx) {
                    tab.lines.push((msg.clone(), TermLineKind::Error));
                }
                self.chat.push(ChatMessage::error(msg));
            }
            Err(e) => {
                let msg = format!("ไม่สามารถรัน git ได้: {e}");
                self.chat.push(ChatMessage::error(msg));
            }
        }
    }

    fn switch_project(&mut self, new_dir: PathBuf) {
        if !new_dir.is_dir() {
            return;
        }
        let _ = std::env::set_current_dir(&new_dir);
        self.cwd = new_dir.clone();
        if !self.recent_projects.iter().any(|p| p == &new_dir) {
            self.recent_projects.insert(0, new_dir.clone());
        }
        self.selected_change_idx = 0;
        self.selected_commit_hash = None;
        self.selected_commit_changes.clear();
        self.selected_commit_file_idx = 0;
        self.file_filter.clear();
        self.refresh_changes();

        let msg = format!("สลับโปรเจกต์ไปที่: {}", new_dir.display());
        self.ai_has_session = false;
        self.ai_conversation_id = None;
        self.chat.push(ChatMessage::system(msg.clone()));
        if let Some(tab) = self.term_tabs.get_mut(self.active_term_idx) {
            tab.cwd = new_dir;
            tab.refresh_branch();
            tab.lines.push((msg, TermLineKind::Command));
        }
    }

    fn refresh_providers(&self) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let list = ai::detect_providers().await;
            let _ = tx.send(BgEvent::ProvidersLoaded(list));
            ctx.request_repaint();
        });
    }

    fn fetch_provider_capabilities(&self, provider_id: String) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let (models, agents) = tokio::join!(
                ai::fetch_models_for_provider(&provider_id),
                ai::fetch_agents_for_provider(&provider_id)
            );
            let _ = tx.send(BgEvent::ModelsAndAgentsLoaded { models, agents });
            ctx.request_repaint();
        });
    }

    fn poll_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                BgEvent::ProvidersLoaded(list) => {
                    let available: Vec<String> = list
                        .iter()
                        .filter(|p| p.available)
                        .map(|p| p.name.clone())
                        .collect();
                    self.providers = list;
                    if self.active_provider().is_none() {
                        self.selected_provider_id =
                            ai::select_provider(&self.providers, None).map(|p| p.id.clone());
                    }
                    if let Some(pid) = self.active_provider().map(|p| p.id.clone()) {
                        self.fetch_provider_capabilities(pid);
                    }
                    if available.is_empty() {
                        self.chat.push(ChatMessage::system(
                            "ยังไม่พบ AI CLI / API Key ในเครื่อง (รองรับ OPENAI_API_KEY, ANTHROPIC_API_KEY, GEMINI_API_KEY, Ollama, agy, claude, codex)",
                        ));
                    }
                }
                BgEvent::ModelsAndAgentsLoaded { models, agents } => {
                    if !models.is_empty() {
                        let grouped = ai::group_models_by_base(&models);
                        if !grouped.iter().any(|(b, _)| b == &self.selected_base_model) {
                            if let Some((b, lvls)) = grouped.first() {
                                self.selected_base_model = b.clone();
                                self.selected_level = lvls.first().cloned();
                            }
                        } else if let Some((_, lvls)) =
                            grouped.iter().find(|(b, _)| b == &self.selected_base_model)
                        {
                            if let Some(cur_lvl) = &self.selected_level {
                                if !lvls.iter().any(|l| l == cur_lvl) {
                                    self.selected_level = lvls.first().cloned();
                                }
                            } else {
                                self.selected_level = lvls.first().cloned();
                            }
                        }
                        self.raw_models = models;
                        self.grouped_models = grouped;
                    }
                    if !agents.is_empty() {
                        if !agents.iter().any(|a| a == &self.selected_agent) {
                            self.selected_agent = agents[0].clone();
                        }
                        self.available_agents = agents;
                    }
                }
                BgEvent::AiStream(stream_ev) => match stream_ev {
                    AiStreamEvent::Status(s) => {
                        self.ai_status = s;
                    }
                    AiStreamEvent::Thought(snippet) => {
                        self.ai_live_thought = Some(snippet);
                    }
                    AiStreamEvent::ConversationId(cid) => {
                        self.ai_conversation_id = Some(cid);
                    }
                    AiStreamEvent::Activity { done, verb, detail } => {
                        if done {
                            // Mark matching in-progress activity as done, or append completed one
                            if let Some(existing) = self
                                .ai_live_activities
                                .iter_mut()
                                .rev()
                                .find(|(d, _, det)| !*d && (det == &detail || detail.is_empty()))
                            {
                                existing.0 = true;
                                existing.1 = verb;
                                if !detail.is_empty() {
                                    existing.2 = detail;
                                }
                            } else {
                                self.ai_live_activities.push((true, verb, detail));
                            }
                            self.refresh_changes();
                        } else {
                            self.ai_live_activities.push((false, verb, detail));
                        }
                    }
                },
                BgEvent::AiReply {
                    provider,
                    model,
                    text,
                    duration_ms,
                } => {
                    self.ai_busy = false;
                    self.ai_has_session = true;
                    self.ai_status.clear();
                    self.ai_started_at = None;
                    let completed_acts: Vec<(String, String)> = self
                        .ai_live_activities
                        .drain(..)
                        .map(|(_, verb, detail)| {
                            let done_verb = match verb.as_str() {
                                "Reading" => "Read".into(),
                                "Editing" => "Edited".into(),
                                "Running" => "Ran".into(),
                                "Searching" => "Searched".into(),
                                _ => verb,
                            };
                            (done_verb, detail)
                        })
                        .collect();
                    let thought_part = self.ai_live_thought.take();
                    let secs = duration_ms as f64 / 1000.0;
                    let est_tok = (text.chars().count() / 4).max(1);
                    let meta = match thought_part {
                        Some(th) => format!("Thought for {secs:.1}s ({th}) · {model}"),
                        None => format!("Thought for {secs:.1}s, ~{est_tok} tokens · {model}"),
                    };
                    self.chat
                        .push(ChatMessage::ai(provider, text, Some(meta), completed_acts));
                    self.refresh_changes();
                }
                BgEvent::AiError(err) => {
                    self.ai_busy = false;
                    self.ai_status.clear();
                    self.ai_started_at = None;
                    self.ai_live_thought = None;
                    self.ai_live_activities.clear();
                    self.chat.push(ChatMessage::error(err));
                }
                BgEvent::RuneResult(out) => {
                    self.ai_busy = false;
                    self.ai_status.clear();
                    self.ai_started_at = None;
                    self.chat.push(ChatMessage::ai(
                        "rune",
                        out,
                        Some("Rune VM".into()),
                        Vec::new(),
                    ));
                    self.refresh_changes();
                }
                BgEvent::TerminalLine {
                    tab_id,
                    line,
                    is_err,
                } => {
                    let max_lines = self.settings.max_terminal_lines.max(50);
                    if let Some(tab) = self.term_tabs.iter_mut().find(|t| t.id == tab_id) {
                        let kind = if is_err {
                            TermLineKind::Error
                        } else {
                            TermLineKind::Output
                        };
                        tab.lines.push((line, kind));
                        if tab.lines.len() > max_lines {
                            let excess = tab.lines.len() - max_lines;
                            tab.lines.drain(0..excess);
                        }
                    }
                }
                BgEvent::TerminalDone { tab_id } => {
                    if let Some(tab) = self.term_tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.busy = false;
                        tab.cancel_tx = None;
                        tab.refresh_branch();
                    }
                    self.refresh_changes();
                }
                BgEvent::ConfigReloaded(cfg) => {
                    self.live_cfg = cfg;
                    self.config_reloads += 1;
                    self.config_error = None;
                }
                BgEvent::ConfigError(err) => {
                    self.config_error = Some(err);
                }
            }
        }
    }

    fn theme(&self) -> ThemeColors {
        if self.dark_mode {
            ThemeColors::dark()
        } else {
            ThemeColors::light()
        }
    }

    fn accent_color(&self) -> Color32 {
        let t = self.theme();
        if self.dark_mode {
            parse_hex_color(&self.live_cfg.accent_hex, t.accent_indigo)
        } else {
            t.accent_indigo
        }
    }

    fn canvas_bg(&self) -> Color32 {
        let t = self.theme();
        if self.dark_mode {
            parse_hex_color(&self.live_cfg.bg_canvas_hex, t.bg_canvas)
        } else {
            t.bg_canvas
        }
    }

    fn panel_bg(&self) -> Color32 {
        let t = self.theme();
        if self.dark_mode {
            parse_hex_color(&self.live_cfg.bg_panel_hex, t.bg_panel)
        } else {
            t.bg_panel
        }
    }

    fn send_prompt(&mut self, force_rune: bool) {
        let input = self.prompt_input.trim().to_string();
        if input.is_empty() && self.attached_images.is_empty() {
            return;
        }
        self.prompt_input.clear();
        let attachments = std::mem::take(&mut self.attached_images);

        if force_rune {
            self.run_rune_inline(input);
            return;
        }

        if input == "/ai" {
            let available: Vec<&AiProvider> =
                self.providers.iter().filter(|p| p.available).collect();
            let content = if available.is_empty() {
                "ไม่พบ AI ที่พร้อมใช้งานในเครื่อง".to_string()
            } else {
                let mut lines = vec!["AI ที่พร้อมใช้งานในเครื่อง:".to_string()];
                for p in available {
                    lines.push(format!("  • {} — {}", p.name, p.detail));
                }
                lines.join("\n")
            };
            self.chat.push(ChatMessage::system(content));
            self.refresh_providers();
            return;
        }

        if input == "/clear" {
            self.chat.clear();
            self.ai_has_session = false;
            self.ai_conversation_id = None;
            return;
        }

        if let Some(code) = input.strip_prefix("/rune ") {
            self.run_rune_inline(code.trim().to_string());
            return;
        }

        if let Some(file) = input.strip_prefix("/run ") {
            let path = self.cwd.join(file.trim());
            self.run_rune_file(path);
            return;
        }

        let attachment_names: Vec<String> = attachments
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();

        let display_text = if input.is_empty() {
            "(แนบรูปภาพ)".to_string()
        } else {
            input.clone()
        };

        self.chat.push(ChatMessage::user(display_text, attachment_names));

        let Some(provider) = self.active_provider().cloned() else {
            self.chat.push(ChatMessage::error(
                "ยังไม่มี AI provider ที่พร้อมใช้งาน — ตรวจสอบการตั้งค่าหรือกด Refresh AI",
            ));
            return;
        };

        let prompt_to_send = if input.is_empty() {
            "ช่วยดูรูปที่แนบมานี้และอธิบายหรือช่วยแก้ไขให้หน่อย".to_string()
        } else {
            input
        };

        self.ai_busy = true;
        self.ai_status = "Loading...".to_string();
        self.ai_started_at = Some(std::time::Instant::now());
        self.ai_live_thought = None;
        self.ai_live_activities.clear();
        let full_model = ai::resolve_full_model_id(
            &self.raw_models,
            &self.selected_base_model,
            self.selected_level.as_deref(),
        );
        let opts = AiRequestOptions {
            model: Some(full_model),
            agent: Some(self.selected_agent.clone()),
            effort: self.selected_effort,
            mode: Some(self.settings.execution_mode.clone()),
            system_instruction: if self.settings.system_instruction.trim().is_empty() {
                None
            } else {
                Some(self.settings.system_instruction.clone())
            },
            timeout_secs: self.settings.request_timeout_secs,
            attachments,
            continue_conversation: self.ai_has_session,
            conversation_id: self.ai_conversation_id.clone(),
            working_dir: Some(self.cwd.clone()),
        };
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let started = std::time::Instant::now();
        self.rt.spawn(async move {
            let tx_ev = tx.clone();
            let ctx_ev = ctx.clone();
            match ai::ask_provider_streaming(&provider, &prompt_to_send, &opts, move |ev| {
                let _ = tx_ev.send(BgEvent::AiStream(ev));
                ctx_ev.request_repaint();
            })
            .await
            {
                Ok(reply) => {
                    let model_label = opts.model.as_deref().unwrap_or("Auto").to_string();
                    let _ = tx.send(BgEvent::AiReply {
                        provider: provider.name,
                        model: model_label,
                        text: reply,
                        duration_ms: started.elapsed().as_millis(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::AiError(format!("AI error: {e}")));
                }
            }
            ctx.request_repaint();
        });
    }

    fn run_rune_inline(&mut self, code: String) {
        self.chat
            .push(ChatMessage::user(format!("/rune {code}"), Vec::new()));
        self.ai_busy = true;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            match rune_engine::eval_inline(&code).await {
                Ok(out) => {
                    let _ = tx.send(BgEvent::RuneResult(out));
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::AiError(format!("Rune error: {e}")));
                }
            }
            ctx.request_repaint();
        });
    }

    fn run_rune_file(&mut self, path: PathBuf) {
        self.chat.push(ChatMessage::user(
            format!("/run {}", path.display()),
            Vec::new(),
        ));
        self.ai_busy = true;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            match rune_engine::run_file(&path).await {
                Ok(out) => {
                    let _ = tx.send(BgEvent::RuneResult(out));
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::AiError(format!("Rune file error: {e}")));
                }
            }
            ctx.request_repaint();
        });
    }

    fn run_terminal_cmd(&mut self, cmd_override: Option<String>) {
        let Some(tab) = self.term_tabs.get_mut(self.active_term_idx) else {
            return;
        };

        let cmd = match cmd_override {
            Some(c) => c,
            None => {
                let c = tab.input.trim().to_string();
                tab.input.clear();
                c
            }
        };
        if cmd.is_empty() {
            return;
        }

        // Save to command history
        if tab.history.last().map(|s| s.as_str()) != Some(cmd.as_str()) {
            tab.history.push(cmd.clone());
        }
        tab.history_idx = None;

        // Echo the prompt + command in Git Bash style
        let short_path = format_git_bash_path(&tab.cwd);
        let prompt_prefix = match &tab.git_branch {
            Some(b) => format!("{short_path} ({b}) $ {cmd}"),
            None => format!("{short_path} $ {cmd}"),
        };
        tab.lines.push((prompt_prefix, TermLineKind::Command));

        if cmd == "clear" {
            tab.lines.clear();
            return;
        }

        // Handle built-in `cd` so directory persists per terminal tab
        if cmd == "cd" || cmd.starts_with("cd ") {
            let target = cmd.strip_prefix("cd").unwrap_or("").trim();
            let new_path = if target.is_empty() || target == "~" {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| tab.cwd.clone())
            } else {
                tab.cwd.join(target)
            };
            match std::fs::canonicalize(&new_path) {
                Ok(canon) if canon.is_dir() => {
                    tab.cwd = canon;
                    tab.refresh_branch();
                }
                _ => {
                    tab.lines.push((
                        format!("cd: no such file or directory: {target}"),
                        TermLineKind::Error,
                    ));
                }
            }
            return;
        }

        tab.busy = true;
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        tab.cancel_tx = Some(cancel_tx);
        let tab_id = tab.id;
        let cwd = tab.cwd.clone();
        let shell = self.settings.shell.clone();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let child_res = Command::new(&shell)
                .arg("-c")
                .arg(&cmd)
                .current_dir(&cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn();

            match child_res {
                Ok(mut child) => {
                    // Stream stdout live line-by-line (so `pnpm dev`, `cargo build`, etc. show output immediately!)
                    if let Some(stdout_pipe) = child.stdout.take() {
                        let tx_out = tx.clone();
                        let ctx_out = ctx.clone();
                        tokio::spawn(async move {
                            let mut reader =
                                tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(stdout_pipe));
                            while let Ok(Some(line)) = reader.next_line().await {
                                let clean = strip_ansi_codes(&line);
                                let _ = tx_out.send(BgEvent::TerminalLine {
                                    tab_id,
                                    line: clean,
                                    is_err: false,
                                });
                                ctx_out.request_repaint();
                            }
                        });
                    }

                    // Stream stderr live line-by-line
                    if let Some(stderr_pipe) = child.stderr.take() {
                        let tx_err = tx.clone();
                        let ctx_err = ctx.clone();
                        tokio::spawn(async move {
                            let mut reader =
                                tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(stderr_pipe));
                            while let Ok(Some(line)) = reader.next_line().await {
                                let clean = strip_ansi_codes(&line);
                                let _ = tx_err.send(BgEvent::TerminalLine {
                                    tab_id,
                                    line: clean,
                                    is_err: true,
                                });
                                ctx_err.request_repaint();
                            }
                        });
                    }

                    tokio::select! {
                        _ = child.wait() => {
                            let _ = tx.send(BgEvent::TerminalDone { tab_id });
                        }
                        _ = cancel_rx => {
                            let _ = child.kill().await;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::TerminalLine {
                        tab_id,
                        line: format!("spawn error: {e}"),
                        is_err: true,
                    });
                    let _ = tx.send(BgEvent::TerminalDone { tab_id });
                }
            }
            ctx.request_repaint();
        });
    }

    /// Render the Orca-style Diff body (always wrapped cleanly like a modern code editor).
    fn render_orca_diff(&self, ui: &mut egui::Ui, change: &FileChange) {
        let t = self.theme();
        if change.diff_lines.is_empty() {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("ไม่มีข้อมูล diff สำหรับรายการนี้")
                        .size(13.0)
                        .color(t.text_muted),
                );
            });
            return;
        }

        match self.diff_mode {
            DiffViewMode::Unified => {
                let row_width = ui.available_width();
                let gutter_w = 86.0;
                let code_w = (row_width - gutter_w - 20.0).max(160.0);

                for dl in &change.diff_lines {
                    let (fg, bg, gutter_bg) = match dl.tag {
                        DiffTag::Header => (t.accent_cyan, t.diff_hdr_bg, t.diff_hdr_bg),
                        DiffTag::Added => (t.diff_add_fg, t.diff_add_bg, t.diff_add_gutter),
                        DiffTag::Removed => (t.diff_del_fg, t.diff_del_bg, t.diff_del_gutter),
                        DiffTag::Context => (t.text_secondary, Color32::TRANSPARENT, t.bg_gutter),
                    };

                    let old_s = dl
                        .old_line
                        .map(|n| format!("{n:>4}"))
                        .unwrap_or_else(|| "    ".into());
                    let new_s = dl
                        .new_line
                        .map(|n| format!("{n:>4}"))
                        .unwrap_or_else(|| "    ".into());

                    Frame::new()
                        .fill(bg)
                        .inner_margin(Margin::symmetric(0, 2))
                        .show(ui, |ui| {
                            ui.set_min_width(row_width);
                            ui.horizontal_top(|ui| {
                                Frame::new()
                                    .fill(gutter_bg)
                                    .inner_margin(Margin::symmetric(8, 0))
                                    .show(ui, |ui| {
                                        ui.set_min_width(gutter_w - 16.0);
                                        ui.label(
                                            RichText::new(format!("{old_s} {new_s}"))
                                                .monospace()
                                                .size(11.5)
                                                .color(t.text_muted),
                                        );
                                    });
                                ui.add_space(10.0);
                                ui.vertical(|ui| {
                                    ui.set_max_width(code_w);
                                    let lbl = egui::Label::new(
                                        RichText::new(&dl.content)
                                            .monospace()
                                            .size(self.settings.diff_font_size.clamp(9.0, 22.0))
                                            .color(fg),
                                    );
                                    if self.settings.word_wrap_diff {
                                        ui.add(lbl.wrap());
                                    } else {
                                        ui.add(lbl.extend());
                                    }
                                });
                            });
                        });
                }
            }
            DiffViewMode::Split => {
                let rows = build_split_rows(&change.diff_lines);
                let col_width = ((ui.available_width() - 6.0) / 2.0).max(200.0);

                for row in rows {
                    match row {
                        SplitRow::Header(text) => {
                            Frame::new()
                                .fill(t.diff_hdr_bg)
                                .inner_margin(Margin::symmetric(12, 4))
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(text)
                                                .monospace()
                                                .size(11.5)
                                                .color(t.accent_cyan),
                                        )
                                        .wrap(),
                                    );
                                });
                        }
                        SplitRow::Pair { left, right } => {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                render_split_cell(ui, &t, left, col_width, true);
                                render_split_cell(ui, &t, right, col_width, false);
                            });
                        }
                    }
                }
            }
        }
    }

    /// Bottom-Left: Orca / Antigravity CLI Style AI Transcript & Monospace Prompt
    fn render_bottom_prompt(&mut self, ui: &mut egui::Ui) {
        let t = self.theme();
        let accent = self.accent_color();
        let font_sz = self.settings.ai_font_size.clamp(10.0, 22.0);

        // Minimal top strip (AI + spinner only | Clear right)
        ui.horizontal(|ui| {
            ui_components::section_title(ui, "AI", t.accent_cyan);
            if self.ai_busy {
                ui.add(egui::Spinner::new().size(13.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui_components::ghost_button(ui, "Clear").clicked() {
                    self.chat.clear();
                    self.ai_has_session = false;
                    self.ai_conversation_id = None;
                }
            });
        });
        ui.separator();

        // Bottom Prompt Bar — Orca / Antigravity CLI style: full-width top 1px rule + `> input` + slim status/selector row
        let mut send_clicked = false;
        let mut refresh_ai_clicked = false;
        let has_ai = self.active_provider().is_some();
        let current_ai_label = self
            .active_provider()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "No AI".to_string());
        let available_providers: Vec<(String, String)> = self
            .providers
            .iter()
            .filter(|p| p.available)
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();

        egui::TopBottomPanel::bottom("orca_cli_prompt_bar")
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(t.bg_code)
                    .inner_margin(Margin {
                        left: 2,
                        right: 2,
                        top: 6,
                        bottom: 2,
                    }),
            )
            .show_inside(ui, |ui| {
                // Attached image chips above `>` line (if any)
                if !self.attached_images.is_empty() {
                    let mut remove_idx: Option<usize> = None;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                        for (idx, path) in self.attached_images.iter().enumerate() {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.display().to_string());
                            Frame::new()
                                .fill(t.bg_surface)
                                .stroke(Stroke::new(1.0, t.border_hairline))
                                .corner_radius(CornerRadius::same(5))
                                .inner_margin(Margin::symmetric(6, 2))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        ui_components::icon_image(ui, t.accent_cyan, 12.0);
                                        ui.label(
                                            RichText::new(&name)
                                                .monospace()
                                                .size(11.0)
                                                .color(t.text_secondary),
                                        );
                                        if ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new("×")
                                                        .size(11.5)
                                                        .color(t.text_muted),
                                                )
                                                .sense(egui::Sense::click()),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                                            .clicked()
                                        {
                                            remove_idx = Some(idx);
                                        }
                                    });
                                });
                        }
                    });
                    if let Some(i) = remove_idx {
                        self.attached_images.remove(i);
                    }
                    ui.add_space(4.0);
                }

                // Line 1: Exact Orca CLI `> ` prompt line
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(
                        RichText::new(">")
                            .monospace()
                            .strong()
                            .size(font_sz + 0.5)
                            .color(t.accent_indigo),
                    );
                    let resp = ui.add_sized(
                        [(ui.available_width() - 4.0).max(80.0), (font_sz + 10.0).max(24.0)],
                        TextEdit::multiline(&mut self.prompt_input)
                            .frame(false)
                            .font(egui::FontId::monospace(font_sz))
                            .desired_rows(1)
                            .text_color(t.text_primary),
                    );

                    if resp.has_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                    {
                        if self.prompt_input.ends_with('\n') {
                            self.prompt_input.pop();
                        }
                        send_clicked = true;
                    }
                });

                ui.add_space(4.0);

                // Line 2: Slim muted CLI status/selector row (Image | Provider | Model | Level | Agent | Send)
                let mut switched_provider_id: Option<String> = None;
                let composer_w = ui.available_width();
                let very_compact = composer_w < 420.0;
                let compact = composer_w < 560.0;
                let provider_w = if very_compact {
                    88.0
                } else if compact {
                    110.0
                } else {
                    135.0
                };
                let model_w = if very_compact {
                    88.0
                } else if compact {
                    110.0
                } else {
                    135.0
                };
                let level_w = if very_compact { 56.0 } else { 70.0 };
                let agent_w = if very_compact { 68.0 } else { 86.0 };

                let mut attach_clicked = false;
                let mut selectors_changed = false;
                let mut render_selectors = |ui: &mut egui::Ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if ui_components::ghost_icon_button(ui, "Image", ui_components::icon_image)
                        .on_hover_text("แนบรูปภาพ (PNG, JPG, WebP, GIF)")
                        .clicked()
                    {
                        attach_clicked = true;
                    }
                    let dot_color = if has_ai { t.accent_emerald } else { t.accent_amber };
                    ui_components::status_dot(ui, dot_color, 3.5);

                    egui::ComboBox::from_id_salt("gemini_ai_selector")
                        .selected_text(
                            RichText::new(&current_ai_label)
                                .monospace()
                                .size(11.0)
                                .color(t.text_secondary),
                        )
                        .width(provider_w)
                        .show_ui(ui, |ui| {
                            if available_providers.is_empty() {
                                ui.label(
                                    RichText::new("ไม่พบ AI ที่พร้อมใช้งาน")
                                        .size(11.5)
                                        .color(t.text_muted),
                                );
                            } else {
                                for (id, name) in &available_providers {
                                    let selected =
                                        self.selected_provider_id.as_deref() == Some(id.as_str());
                                    if ui.selectable_label(selected, name).clicked() && !selected {
                                        self.selected_provider_id = Some(id.clone());
                                        switched_provider_id = Some(id.clone());
                                        selectors_changed = true;
                                    }
                                }
                            }
                            ui.separator();
                            if ui.button("Refresh AI").clicked() {
                                refresh_ai_clicked = true;
                            }
                        });

                    let mut newly_selected_base: Option<(String, Option<String>)> = None;
                    let model_display = if self.selected_base_model.is_empty() {
                        "Default".to_string()
                    } else {
                        self.selected_base_model.clone()
                    };
                    egui::ComboBox::from_id_salt("gemini_model_selector")
                        .selected_text(
                            RichText::new(&model_display)
                                .monospace()
                                .size(11.0)
                                .color(t.text_secondary),
                        )
                        .width(model_w)
                        .show_ui(ui, |ui| {
                            if self.grouped_models.is_empty() {
                                ui.label(
                                    RichText::new("ไม่พบรายชื่อโมเดลจาก Provider นี้")
                                        .size(11.5)
                                        .color(t.text_muted),
                                );
                            } else {
                                for (base, lvls) in &self.grouped_models {
                                    let selected = self.selected_base_model == *base;
                                    if ui.selectable_label(selected, base).clicked() {
                                        let default_lvl = lvls
                                            .iter()
                                            .find(|l| l.as_str() == "medium")
                                            .or_else(|| lvls.first())
                                            .cloned();
                                        newly_selected_base = Some((base.clone(), default_lvl));
                                    }
                                }
                            }
                        });
                    if let Some((base, lvl)) = newly_selected_base {
                        self.selected_base_model = base;
                        self.selected_level = lvl;
                        selectors_changed = true;
                    }

                    let current_levels: Vec<String> = self
                        .grouped_models
                        .iter()
                        .find(|(b, _)| b == &self.selected_base_model)
                        .map(|(_, lvls)| lvls.clone())
                        .unwrap_or_default();

                    if !current_levels.is_empty() {
                        let cur_lvl = self
                            .selected_level
                            .clone()
                            .unwrap_or_else(|| current_levels[0].clone());
                        let lvl_color = match cur_lvl.as_str() {
                            "low" => t.text_secondary,
                            "medium" => t.accent_amber,
                            "high" => t.accent_violet,
                            _ => t.accent_rose,
                        };
                        egui::ComboBox::from_id_salt("gemini_effort_selector")
                            .selected_text(
                                RichText::new(&cur_lvl)
                                    .monospace()
                                    .size(11.0)
                                    .color(lvl_color),
                            )
                            .width(level_w)
                            .show_ui(ui, |ui| {
                                for lvl in &current_levels {
                                    let selected = cur_lvl == *lvl;
                                    if ui.selectable_label(selected, lvl).clicked() {
                                        self.selected_level = Some(lvl.clone());
                                        selectors_changed = true;
                                    }
                                }
                            });
                    }

                    egui::ComboBox::from_id_salt("gemini_agent_selector")
                        .selected_text(
                            RichText::new(&self.selected_agent)
                                .monospace()
                                .size(11.0)
                                .color(t.accent_cyan),
                        )
                        .width(agent_w)
                        .show_ui(ui, |ui| {
                            for a in &self.available_agents {
                                let selected = self.selected_agent == *a;
                                if ui.selectable_label(selected, a).clicked() {
                                    self.selected_agent = a.clone();
                                    selectors_changed = true;
                                }
                            }
                        });
                };

                let can_send = (!self.prompt_input.trim().is_empty()
                    || !self.attached_images.is_empty())
                    && !self.ai_busy;

                if composer_w < 580.0 {
                    ui.horizontal_wrapped(|ui| render_selectors(ui));
                    ui.add_space(4.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_components::primary_button(ui, "Send", accent, can_send).clicked() {
                            send_clicked = true;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        render_selectors(ui);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui_components::primary_button(ui, "Send", accent, can_send).clicked()
                            {
                                send_clicked = true;
                            }
                        });
                    });
                }

                if attach_clicked {
                    if let Some(files) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif"])
                        .pick_files()
                    {
                        for f in files {
                            if !self.attached_images.contains(&f) {
                                self.attached_images.push(f);
                            }
                        }
                    }
                }

                if selectors_changed {
                    self.persist_ui_state();
                }

                if let Some(pid) = switched_provider_id {
                    self.fetch_provider_capabilities(pid);
                }
            });

        if refresh_ai_clicked {
            self.refresh_providers();
        }
        if send_clicked {
            self.send_prompt(false);
        }

        // Orca / Antigravity CLI Style Monospace Transcript Feed (no chat bubbles!)
        egui::ScrollArea::vertical()
            .id_salt("bottom_chat_scroll")
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.add_space(4.0);

                for (idx, msg) in self.chat.iter().enumerate() {
                    match msg.role.as_str() {
                        "you" => {
                            // Thin horizontal divider above each user prompt (just like Orca CLI)
                            if idx > 0 {
                                ui.add_space(6.0);
                            }
                            let sep_rect = ui.available_rect_before_wrap();
                            let line_w = (sep_rect.width() * 0.48).min(380.0);
                            ui.painter().line_segment(
                                [
                                    egui::pos2(sep_rect.left(), sep_rect.top()),
                                    egui::pos2(sep_rect.left() + line_w, sep_rect.top()),
                                ],
                                Stroke::new(1.0, t.border_subtle),
                            );
                            ui.add_space(5.0);

                            // `> <prompt>` in crisp blue bold monospace
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(
                                    RichText::new(">")
                                        .monospace()
                                        .strong()
                                        .size(font_sz)
                                        .color(t.accent_indigo),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&msg.content)
                                            .monospace()
                                            .strong()
                                            .size(font_sz)
                                            .color(t.accent_indigo),
                                    )
                                    .wrap(),
                                );
                            });

                            // Attached image summary line (`▸ [icon] N image(s): ...`)
                            if !msg.attachments.is_empty() {
                                ui.horizontal(|ui| {
                                    ui.add_space(12.0);
                                    ui.label(
                                        RichText::new("▸")
                                            .monospace()
                                            .size(font_sz - 1.0)
                                            .color(t.text_muted),
                                    );
                                    ui_components::icon_image(ui, t.text_muted, 12.0);
                                    ui.label(
                                        RichText::new(format!(
                                            "{} image(s) ({})",
                                            msg.attachments.len(),
                                            msg.attachments.join(", ")
                                        ))
                                        .monospace()
                                        .size(font_sz - 1.0)
                                        .color(t.text_muted),
                                    );
                                });
                            }
                            ui.add_space(4.0);
                        }
                        "system" => {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("• {}", msg.content))
                                        .monospace()
                                        .size(font_sz - 1.0)
                                        .color(t.text_muted),
                                )
                                .wrap(),
                            );
                            ui.add_space(4.0);
                        }
                        "error" => {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("✗ {}", msg.content))
                                        .monospace()
                                        .size(font_sz - 0.5)
                                        .color(t.accent_rose),
                                )
                                .wrap(),
                            );
                            ui.add_space(6.0);
                        }
                        _ => {
                            // Completed thought summary (`▸ Thought for 17.2s, ~188 tokens · model`)
                            if let Some(meta) = &msg.meta {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("▸ {meta}"))
                                            .monospace()
                                            .size(font_sz - 1.0)
                                            .color(t.text_muted),
                                    )
                                    .wrap(),
                                );
                            }

                            // Completed tool activities (`● Edited desktop.rs`, `● Read ai.rs`)
                            for (verb, detail) in &msg.activities {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui_components::status_dot(ui, t.accent_emerald, 3.5);
                                    ui.label(
                                        RichText::new(verb)
                                            .monospace()
                                            .strong()
                                            .size(font_sz - 0.5)
                                            .color(t.accent_amber),
                                    );
                                    if !detail.is_empty() {
                                        ui.label(
                                            RichText::new(format!("({detail})"))
                                                .monospace()
                                                .size(font_sz - 0.5)
                                                .color(t.text_secondary),
                                        );
                                    }
                                });
                            }

                            if msg.meta.is_some() || !msg.activities.is_empty() {
                                ui.add_space(4.0);
                            }

                            // Rich monospace markdown-aware rendering (Orca CLI look!)
                            render_cli_markdown(ui, &t, &msg.content, font_sz);
                            ui.add_space(8.0);
                        }
                    }
                }

                // Live streaming status & activities while AI is working (exact Orca / Antigravity CLI style)
                if self.ai_busy {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(90));
                    let elapsed = self
                        .ai_started_at
                        .map(|s| s.elapsed().as_secs_f32())
                        .unwrap_or(0.0);

                    // 1. Live Thought line (`○ Thought for 4.2s (...)`)
                    if let Some(thought) = &self.ai_live_thought {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let (r, _) =
                                ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            ui.painter().circle_stroke(
                                r.center(),
                                3.5,
                                Stroke::new(1.2, t.text_muted),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "Thought for {elapsed:.1}s ({thought}...)"
                                    ))
                                    .monospace()
                                    .size(font_sz - 1.0)
                                    .color(t.text_muted),
                                )
                                .wrap(),
                            );
                        });
                    }

                    // 2. Live Tool Activities (color-coded by real action: Reading / Editing / Running / Searching)
                    for (done, verb, detail) in &self.ai_live_activities {
                        let lower_v = verb.to_lowercase();
                        let verb_color = if lower_v.starts_with("read") {
                            t.accent_cyan
                        } else if lower_v.starts_with("edit") {
                            t.accent_amber
                        } else if lower_v.starts_with("run") || lower_v.starts_with("ran") {
                            t.accent_emerald
                        } else if lower_v.starts_with("search") {
                            t.accent_indigo
                        } else {
                            t.accent_violet
                        };
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if *done {
                                ui_components::status_dot(ui, t.accent_emerald, 3.5);
                            } else {
                                let (r, _) = ui.allocate_exact_size(
                                    Vec2::splat(10.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_stroke(
                                    r.center(),
                                    3.5,
                                    Stroke::new(1.3_f32, verb_color),
                                );
                            }
                            ui.label(
                                RichText::new(verb)
                                    .monospace()
                                    .strong()
                                    .size(font_sz - 0.5)
                                    .color(verb_color),
                            );
                            if !detail.is_empty() {
                                ui.label(
                                    RichText::new(detail)
                                        .monospace()
                                        .size(font_sz - 0.5)
                                        .color(t.text_secondary),
                                );
                            }
                        });
                    }

                    // 3. Vector-drawn 2x4 animated dot-matrix spinner + real color-coded phase badge
                    ui.add_space(3.0);
                    let status_text = if self.ai_status.is_empty() {
                        "Loading...".to_string()
                    } else {
                        self.ai_status.clone()
                    };
                    let lower_s = status_text.to_lowercase();
                    let (phase_label, phase_color) = if lower_s.contains("edit") {
                        ("Editing", t.accent_amber)
                    } else if lower_s.contains("read") {
                        ("Reading", t.accent_cyan)
                    } else if lower_s.contains("think") {
                        ("Thinking", t.accent_violet)
                    } else if lower_s.contains("run") {
                        ("Running", t.accent_emerald)
                    } else if lower_s.contains("search") {
                        ("Searching", t.accent_indigo)
                    } else if lower_s.contains("writ") {
                        ("Writing", t.accent_indigo)
                    } else {
                        ("Loading", t.text_secondary)
                    };

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let (spin_rect, _) =
                            ui.allocate_exact_size(Vec2::new(10.0, 14.0), egui::Sense::hover());
                        let p = ui.painter();
                        let step = ((elapsed * 10.0) as usize) % 8;
                        let order: [(f32, f32); 8] = [
                            (0.25, 0.16),
                            (0.75, 0.16),
                            (0.75, 0.39),
                            (0.75, 0.62),
                            (0.75, 0.85),
                            (0.25, 0.85),
                            (0.25, 0.62),
                            (0.25, 0.39),
                        ];
                        for (i, (fx, fy)) in order.iter().enumerate() {
                            let dist = (step + 8 - i) % 8;
                            let color = if dist == 0 {
                                phase_color
                            } else if dist <= 2 {
                                t.text_secondary
                            } else {
                                t.border_subtle
                            };
                            let pt = egui::pos2(
                                spin_rect.left() + spin_rect.width() * fx,
                                spin_rect.top() + spin_rect.height() * fy,
                            );
                            p.circle_filled(pt, 1.3, color);
                        }
                        ui.label(
                            RichText::new(phase_label)
                                .monospace()
                                .strong()
                                .size(font_sz - 0.5)
                                .color(phase_color),
                        );
                        ui.label(
                            RichText::new(format!("{status_text} ({elapsed:.1}s)"))
                                .monospace()
                                .size(font_sz - 0.5)
                                .color(t.text_secondary),
                        );
                    });
                }
            });
    }

    /// Bottom-Right: VS Code-Style Integrated Terminal (Tabs + Inline Shell Prompt + History)
    fn render_bottom_terminal(&mut self, ui: &mut egui::Ui) {
        let t = self.theme();
        let mut switch_to: Option<usize> = None;
        let mut close_idx: Option<usize> = None;
        let mut add_new_tab = false;
        let mut clear_active = false;

        // 1. Unified Terminal Header Bar (matches AI CHAT header symmetrically)
        ui.horizontal(|ui| {
            ui_components::section_title(ui, "TERMINAL", t.accent_emerald);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui_components::ghost_button(ui, "Clear")
                    .on_hover_text("Clear terminal output")
                    .clicked()
                {
                    clear_active = true;
                }
                if ui_components::subtle_button(ui, "+")
                    .on_hover_text("New Terminal (zsh)")
                    .clicked()
                {
                    add_new_tab = true;
                }
            });
        });

        ui.separator();

        // 2. VS Code-Style Right Sidebar for Terminal Tabs (auto-hides if terminal pane is very narrow)
        let show_term_sidebar = ui.available_width() >= 340.0;
        if show_term_sidebar {
            let sidebar_w = if ui.available_width() < 460.0 {
                104.0
            } else {
                124.0
            };
            egui::SidePanel::right("term_sessions_sidebar")
                .resizable(false)
                .exact_width(sidebar_w)
                .show_separator_line(true)
                .frame(
                    Frame::new()
                        .fill(t.bg_code)
                        .inner_margin(Margin {
                            left: 8,
                            right: 4,
                            top: 4,
                            bottom: 4,
                        }),
                )
                .show_inside(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    egui::ScrollArea::vertical()
                        .id_salt("term_tabs_list_scroll")
                        .show(ui, |ui| {
                            let row_w = (ui.available_width() - 2.0).max(70.0);
                            for (idx, tab) in self.term_tabs.iter().enumerate() {
                                let is_active = idx == self.active_term_idx;
                                let bg = if is_active {
                                    t.bg_surface
                                } else {
                                    Color32::TRANSPARENT
                                };
                                let fg = if is_active { t.text_primary } else { t.text_muted };
                                let stroke = if is_active {
                                    Stroke::new(1.0, t.border_subtle)
                                } else {
                                    Stroke::NONE
                                };

                                Frame::new()
                                    .fill(bg)
                                    .stroke(stroke)
                                    .corner_radius(CornerRadius::same(5))
                                    .inner_margin(Margin::symmetric(7, 5))
                                    .show(ui, |ui| {
                                        ui.set_width((row_w - 14.0).max(56.0));
                                        ui.horizontal(|ui| {
                                            if ui
                                                .add(
                                                    egui::Label::new(
                                                        RichText::new(format!(">_ {}", tab.name))
                                                            .size(11.0)
                                                            .monospace()
                                                            .color(fg),
                                                    )
                                                    .truncate()
                                                    .sense(egui::Sense::click()),
                                                )
                                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                                .clicked()
                                            {
                                                switch_to = Some(idx);
                                            }
                                            if self.term_tabs.len() > 1 {
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        if ui
                                                            .add(
                                                                egui::Label::new(
                                                                    RichText::new("×")
                                                                        .size(11.5)
                                                                        .color(t.text_muted),
                                                                )
                                                                .sense(egui::Sense::click()),
                                                            )
                                                            .on_hover_cursor(
                                                                egui::CursorIcon::PointingHand,
                                                            )
                                                            .clicked()
                                                        {
                                                            close_idx = Some(idx);
                                                        }
                                                    },
                                                );
                                            }
                                        });
                                    });
                            }
                        });
                });
        }

        if let Some(idx) = switch_to {
            self.active_term_idx = idx;
        }
        if add_new_tab {
            let id = self.next_term_id;
            self.next_term_id += 1;
            self.term_tabs
                .push(TermTab::new(id, self.cwd.clone(), self.ctx.clone()));
            self.active_term_idx = self.term_tabs.len() - 1;
        }
        if let Some(idx) = close_idx {
            if self.term_tabs.len() > 1 {
                self.term_tabs.remove(idx);
                if self.active_term_idx >= self.term_tabs.len() {
                    self.active_term_idx = self.term_tabs.len() - 1;
                }
            }
        }
        if clear_active {
            if let Some(tab) = self.term_tabs.get_mut(self.active_term_idx) {
                if let Some(pty) = &mut tab.pty {
                    pty.clear();
                } else {
                    tab.lines.clear();
                }
            }
        }

        // 3. Real VS Code-style Interactive PTY Terminal inside CentralPanel
        let accent = self.accent_color();
        let term_font_size = self.settings.terminal_font_size.clamp(10.0, 22.0);
        let ctx_clone = self.ctx.clone();
        let mut restart_active = false;

        egui::CentralPanel::default()
            .frame(Frame::new().fill(t.bg_code).inner_margin(Margin {
                left: 0,
                right: 4,
                top: 2,
                bottom: 0,
            }))
            .show_inside(ui, |ui| {
                if let Some(tab) = self.term_tabs.get_mut(self.active_term_idx) {
                    let exited = tab
                        .pty
                        .as_ref()
                        .and_then(|p| p.exited.lock().ok().map(|g| *g))
                        .unwrap_or(false);

                    if let Some(err) = &tab.pty_error {
                        ui.label(
                            RichText::new(err)
                                .monospace()
                                .size(term_font_size)
                                .color(t.accent_rose),
                        );
                    } else if exited {
                        ui.vertical_centered(|ui| {
                            ui.add_space(24.0);
                            ui.label(
                                RichText::new("Shell session exited")
                                    .size(13.0)
                                    .color(t.text_muted),
                            );
                            ui.add_space(8.0);
                            if ui_components::subtle_button(ui, "Restart Shell").clicked() {
                                restart_active = true;
                            }
                        });
                    } else if let Some(pty) = &mut tab.pty {
                        let id_salt = format!("pty_term_tab_{}", tab.id);
                        pty.render(ui, &t, accent, &id_salt, term_font_size);
                    }
                }
            });

        if restart_active {
            if let Some(tab) = self.term_tabs.get_mut(self.active_term_idx) {
                let cwd = tab.cwd.clone();
                let id = tab.id;
                *tab = TermTab::new(id, cwd, ctx_clone);
            }
        }
    }
}

/// Render AI response text in Orca / Antigravity CLI monospace markdown style:
/// headings in bold violet, inline `code` in cyan, **bold** in bright white, bullets with `•`, code fences in surface box.
fn render_cli_markdown(ui: &mut egui::Ui, t: &ThemeColors, text: &str, font_sz: f32) {
    let mut in_code_block = false;
    let mut code_buf: Vec<&str> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_code_block {
                let block = code_buf.join("\n");
                code_buf.clear();
                in_code_block = false;
                Frame::new()
                    .fill(t.bg_surface)
                    .stroke(Stroke::new(1.0, t.border_hairline))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.add(
                            egui::Label::new(
                                RichText::new(&block)
                                    .monospace()
                                    .size(font_sz - 0.5)
                                    .color(t.accent_cyan),
                            )
                            .wrap(),
                        );
                    });
                ui.add_space(3.0);
            } else {
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            code_buf.push(line);
            continue;
        }

        if trimmed.is_empty() {
            ui.add_space(5.0);
            continue;
        }

        // Headings (`### Title`) -> bold violet like Orca CLI
        if let Some(h) = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("## "))
            .or_else(|| trimmed.strip_prefix("# "))
        {
            ui.add_space(3.0);
            ui.add(
                egui::Label::new(
                    RichText::new(h)
                        .monospace()
                        .strong()
                        .size(font_sz)
                        .color(t.accent_violet),
                )
                .wrap(),
            );
            continue;
        }

        // Render line with inline `code` (cyan) and **bold** (bright white) via LayoutJob
        let indent_spaces = line.len() - trimmed.len();
        let (prefix, body) = if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let pad = " ".repeat(indent_spaces.min(8));
            (format!("{pad}• "), rest)
        } else {
            (String::new(), line)
        };

        let mut job = egui::text::LayoutJob::default();
        let mono_font = egui::FontId::monospace(font_sz);

        if !prefix.is_empty() {
            job.append(
                &prefix,
                0.0,
                egui::TextFormat {
                    font_id: mono_font.clone(),
                    color: t.accent_indigo,
                    ..Default::default()
                },
            );
        }

        // Parse inline `code` and **bold** spans
        let mut rest = body;
        while !rest.is_empty() {
            let next_tick = rest.find('`');
            let next_bold = rest.find("**");
            match (next_tick, next_bold) {
                (Some(ti), Some(bi)) if ti < bi => {
                    if ti > 0 {
                        job.append(
                            &rest[..ti],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: t.text_primary,
                                ..Default::default()
                            },
                        );
                    }
                    let after = &rest[ti + 1..];
                    if let Some(end) = after.find('`') {
                        job.append(
                            &after[..end],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: t.accent_cyan,
                                ..Default::default()
                            },
                        );
                        rest = &after[end + 1..];
                    } else {
                        job.append(
                            &rest[ti..],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: t.text_primary,
                                ..Default::default()
                            },
                        );
                        break;
                    }
                }
                (_, Some(bi)) => {
                    if bi > 0 {
                        job.append(
                            &rest[..bi],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: t.text_primary,
                                ..Default::default()
                            },
                        );
                    }
                    let after = &rest[bi + 2..];
                    if let Some(end) = after.find("**") {
                        let bold_color = if ui.visuals().dark_mode {
                            Color32::WHITE
                        } else {
                            Color32::BLACK
                        };
                        job.append(
                            &after[..end],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: bold_color,
                                ..Default::default()
                            },
                        );
                        rest = &after[end + 2..];
                    } else {
                        job.append(
                            &rest[bi..],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: t.text_primary,
                                ..Default::default()
                            },
                        );
                        break;
                    }
                }
                (Some(ti), None) => {
                    if ti > 0 {
                        job.append(
                            &rest[..ti],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: t.text_primary,
                                ..Default::default()
                            },
                        );
                    }
                    let after = &rest[ti + 1..];
                    if let Some(end) = after.find('`') {
                        job.append(
                            &after[..end],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: t.accent_cyan,
                                ..Default::default()
                            },
                        );
                        rest = &after[end + 1..];
                    } else {
                        job.append(
                            &rest[ti..],
                            0.0,
                            egui::TextFormat {
                                font_id: mono_font.clone(),
                                color: t.text_primary,
                                ..Default::default()
                            },
                        );
                        break;
                    }
                }
                (None, None) => {
                    job.append(
                        rest,
                        0.0,
                        egui::TextFormat {
                            font_id: mono_font.clone(),
                            color: t.text_primary,
                            ..Default::default()
                        },
                    );
                    break;
                }
            }
        }

        ui.add(egui::Label::new(job).wrap());
    }
}

/// Draw vector-painted Git Commit Graph lanes (`*`, `|`, `/`, `\`) with distinct lane colors.
fn draw_commit_graph_lanes(ui: &mut egui::Ui, t: &ThemeColors, graph: &str, row_h: f32) {
    let lane_colors = [
        t.accent_indigo,
        t.accent_emerald,
        t.accent_violet,
        t.accent_amber,
        t.accent_cyan,
        t.accent_rose,
    ];
    let chars: Vec<char> = graph.chars().collect();
    let cell_w = 8.0;
    let total_w = (chars.len() as f32 * cell_w).max(16.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(total_w, row_h), egui::Sense::hover());
    let p = ui.painter();
    let y_top = rect.top();
    let y_mid = rect.center().y;
    let y_bot = rect.bottom();

    for (idx, ch) in chars.iter().enumerate() {
        let lane = idx / 2;
        let color = lane_colors[lane % lane_colors.len()];
        let cx = rect.left() + (idx as f32 + 0.5) * cell_w;
        let stroke = Stroke::new(1.8, color);

        match ch {
            '|' => {
                p.line_segment([egui::pos2(cx, y_top), egui::pos2(cx, y_bot)], stroke);
            }
            '*' => {
                p.line_segment([egui::pos2(cx, y_top), egui::pos2(cx, y_bot)], stroke);
                p.circle_filled(egui::pos2(cx, y_mid), 4.0, color);
                p.circle_stroke(egui::pos2(cx, y_mid), 4.0, Stroke::new(1.2, t.bg_panel));
            }
            '/' => {
                p.line_segment(
                    [
                        egui::pos2(cx - cell_w * 0.6, y_bot),
                        egui::pos2(cx + cell_w * 0.6, y_top),
                    ],
                    stroke,
                );
            }
            '\\' => {
                p.line_segment(
                    [
                        egui::pos2(cx - cell_w * 0.6, y_top),
                        egui::pos2(cx + cell_w * 0.6, y_bot),
                    ],
                    stroke,
                );
            }
            '_' | '-' => {
                p.line_segment(
                    [
                        egui::pos2(cx - cell_w * 0.5, y_mid),
                        egui::pos2(cx + cell_w * 0.5, y_mid),
                    ],
                    stroke,
                );
            }
            _ => {}
        }
    }
}

enum SplitRow<'a> {
    Header(&'a str),
    Pair {
        left: Option<&'a DiffLine>,
        right: Option<&'a DiffLine>,
    },
}

fn build_split_rows(lines: &[DiffLine]) -> Vec<SplitRow<'_>> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match lines[i].tag {
            DiffTag::Header => {
                rows.push(SplitRow::Header(&lines[i].content));
                i += 1;
            }
            DiffTag::Context => {
                rows.push(SplitRow::Pair {
                    left: Some(&lines[i]),
                    right: Some(&lines[i]),
                });
                i += 1;
            }
            DiffTag::Removed => {
                let rem_start = i;
                while i < lines.len() && lines[i].tag == DiffTag::Removed {
                    i += 1;
                }
                let add_start = i;
                while i < lines.len() && lines[i].tag == DiffTag::Added {
                    i += 1;
                }
                let rems = &lines[rem_start..add_start];
                let adds = &lines[add_start..i];
                let max_len = rems.len().max(adds.len());
                for k in 0..max_len {
                    rows.push(SplitRow::Pair {
                        left: rems.get(k),
                        right: adds.get(k),
                    });
                }
            }
            DiffTag::Added => {
                rows.push(SplitRow::Pair {
                    left: None,
                    right: Some(&lines[i]),
                });
                i += 1;
            }
        }
    }
    rows
}

fn render_split_cell(
    ui: &mut egui::Ui,
    t: &ThemeColors,
    cell: Option<&DiffLine>,
    width: f32,
    is_left: bool,
) {
    let (bg, gutter_bg, fg, line_no, text) = match cell {
        Some(dl) => match dl.tag {
            DiffTag::Removed => (
                t.diff_del_bg,
                t.diff_del_gutter,
                t.diff_del_fg,
                dl.old_line.map(|n| format!("{n:>4}")).unwrap_or_default(),
                dl.content.as_str(),
            ),
            DiffTag::Added => (
                t.diff_add_bg,
                t.diff_add_gutter,
                t.diff_add_fg,
                dl.new_line.map(|n| format!("{n:>4}")).unwrap_or_default(),
                dl.content.as_str(),
            ),
            _ => (
                Color32::TRANSPARENT,
                t.bg_gutter,
                t.text_secondary,
                if is_left { dl.old_line } else { dl.new_line }
                    .map(|n| format!("{n:>4}"))
                    .unwrap_or_default(),
                dl.content.as_str(),
            ),
        },
        None => (t.bg_canvas, t.bg_gutter, t.text_muted, "    ".into(), ""),
    };

    let code_w = (width - 58.0).max(80.0);

    Frame::new()
        .fill(bg)
        .inner_margin(Margin::symmetric(0, 2))
        .show(ui, |ui| {
            ui.set_min_width(width);
            ui.set_max_width(width);
            ui.horizontal_top(|ui| {
                Frame::new()
                    .fill(gutter_bg)
                    .inner_margin(Margin::symmetric(6, 0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{line_no:>4}"))
                                .monospace()
                                .size(11.5)
                                .color(t.text_muted),
                        );
                    });
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.set_max_width(code_w);
                    ui.add(
                        egui::Label::new(RichText::new(text).monospace().size(12.5).color(fg))
                            .wrap(),
                    );
                });
            });
        });
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();

        // Accept Drag & Drop image files anywhere on the window
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                let is_img = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| matches!(e.to_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif"))
                    .unwrap_or(false);
                if is_img && !self.attached_images.contains(&path) {
                    self.attached_images.push(path);
                }
            }
        }

        // Auto-sync Changed Files based on settings
        if self.settings.auto_refresh_diff {
            let interval = std::time::Duration::from_secs(self.settings.auto_refresh_secs.max(1));
            if self.last_auto_refresh.elapsed() >= interval {
                self.last_auto_refresh = std::time::Instant::now();
                self.refresh_changes();
            }
            ctx.request_repaint_after(interval);
        }

        let t = self.theme();
        let panel_bg = self.panel_bg();
        let canvas_bg = self.canvas_bg();
        let accent = self.accent_color();

        // 1A. Dedicated Themed macOS Titlebar Strip (traffic lights on left, full-width drag + double-click)
        if cfg!(target_os = "macos") {
            let titlebar_resp = egui::TopBottomPanel::top("macos_titlebar_strip")
                .exact_height(28.0)
                .show_separator_line(false)
                .frame(Frame::new().fill(panel_bg))
                .show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new("cli arai")
                                .size(12.0)
                                .strong()
                                .color(t.text_muted),
                        );
                    });
                })
                .response;

            let tb_interact = titlebar_resp.interact(egui::Sense::click_and_drag());
            if tb_interact.double_clicked() {
                let is_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
            } else if tb_interact.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
        }

        // 1B. App Toolbar Row directly below the window controls (full width, clean margins)
        let top_frame = Frame::new()
            .fill(panel_bg)
            .stroke(Stroke::new(1.0, t.border_hairline))
            .inner_margin(Margin {
                left: 16,
                right: 16,
                top: 6,
                bottom: 9,
            });

        egui::TopBottomPanel::top("top_bar")
            .frame(top_frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Brand
                    ui_components::status_dot(ui, accent, 4.5);
                    ui.label(
                        RichText::new("cli arai")
                            .strong()
                            .size(15.5)
                            .color(t.text_primary),
                    );

                    ui.add_space(10.0);
                    // Project Switcher Dropdown + Native Folder Picker
                    let folder_name = self
                        .cwd
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| self.cwd.display().to_string());

                    let narrow_win = ctx.screen_rect().width() < 1120.0;
                    let proj_w = if narrow_win { 130.0 } else { 180.0 };
                    let mut next_project: Option<PathBuf> = None;
                    egui::ComboBox::from_id_salt("project_switcher")
                        .selected_text(
                            RichText::new(format!("📁 {folder_name}"))
                                .size(12.5)
                                .color(t.text_primary),
                        )
                        .width(proj_w)
                        .show_ui(ui, |ui| {
                            if ui
                                .button(
                                    RichText::new("+ Open Folder...")
                                        .strong()
                                        .color(t.accent_cyan),
                                )
                                .clicked()
                            {
                                if let Some(picked) = rfd::FileDialog::new()
                                    .set_directory(&self.cwd)
                                    .pick_folder()
                                {
                                    next_project = Some(picked);
                                }
                            }
                            ui.separator();
                            ui.label(
                                RichText::new("Recent Projects")
                                    .size(11.0)
                                    .color(t.text_muted),
                            );
                            for proj in &self.recent_projects {
                                let name = proj
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| proj.display().to_string());
                                let is_cur = proj == &self.cwd;
                                if ui
                                    .selectable_label(is_cur, format!("📁 {name}"))
                                    .on_hover_text(proj.display().to_string())
                                    .clicked()
                                    && !is_cur
                                {
                                    next_project = Some(proj.clone());
                                }
                            }
                        });

                    if let Some(new_dir) = next_project {
                        self.switch_project(new_dir);
                    }

                    // Git Branch Switcher Dropdown + New Branch Button
                    if let Some(cur_branch) = self.current_branch.clone() {
                        ui.add_space(6.0);
                        let mut switch_branch: Option<String> = None;
                        let branches = self.branches.clone();

                        let branch_w = if narrow_win { 115.0 } else { 155.0 };
                        egui::ComboBox::from_id_salt("git_branch_switcher")
                            .selected_text(
                                RichText::new(format!("git: {cur_branch}"))
                                    .size(12.0)
                                    .monospace()
                                    .color(t.accent_violet),
                            )
                            .width(branch_w)
                            .show_ui(ui, |ui| {
                                if ui
                                    .button(
                                        RichText::new("+ New Branch...")
                                            .strong()
                                            .color(t.accent_emerald),
                                    )
                                    .clicked()
                                {
                                    self.show_new_branch_modal = true;
                                    self.new_branch_input.clear();
                                }
                                ui.separator();
                                ui.label(
                                    RichText::new("Branches")
                                        .size(11.0)
                                        .color(t.text_muted),
                                );
                                if branches.is_empty() {
                                    ui.label(
                                        RichText::new(format!("{cur_branch} (no commits yet)"))
                                            .size(11.5)
                                            .color(t.text_muted),
                                    );
                                } else {
                                    for b in &branches {
                                        let is_cur = b == &cur_branch;
                                        if ui
                                            .selectable_label(
                                                is_cur,
                                                RichText::new(b).monospace().size(12.0),
                                            )
                                            .clicked()
                                            && !is_cur
                                        {
                                            switch_branch = Some(b.clone());
                                        }
                                    }
                                }
                            });

                        if ui_components::subtle_button(ui, "+ Branch")
                            .on_hover_text(format!("แตก branch ใหม่จาก `{cur_branch}`"))
                            .clicked()
                        {
                            self.show_new_branch_modal = true;
                            self.new_branch_input.clear();
                        }

                        if let Some(target_branch) = switch_branch {
                            self.checkout_branch(&target_branch, false);
                        }
                    }

                    // Right-aligned logically grouped controls: [Hide/Show Panel] | [Theme] [Settings]
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Group 2 (Far Right): App Preferences — Settings + Theme
                        if ui_components::ghost_icon_button(
                            ui,
                            "Settings",
                            ui_components::icon_settings,
                        )
                        .on_hover_text("ตั้งค่าโปรแกรม (~/.config/cli-arai/settings.json)")
                        .clicked()
                        {
                            self.show_settings_modal = !self.show_settings_modal;
                            self.settings_status = None;
                        }
                        if self.dark_mode {
                            if ui_components::ghost_icon_button(ui, "Light", ui_components::icon_sun)
                                .on_hover_text("สลับเป็นธีมสว่าง (Light)")
                                .clicked()
                            {
                                self.dark_mode = false;
                                self.settings.dark_mode = false;
                                let _ = self.settings.save();
                                ui_components::apply_theme(ctx, false);
                            }
                        } else if ui_components::ghost_icon_button(
                            ui,
                            "Dark",
                            ui_components::icon_moon,
                        )
                        .on_hover_text("สลับเป็นธีมมืด (Dark)")
                        .clicked()
                        {
                            self.dark_mode = true;
                            self.settings.dark_mode = true;
                            let _ = self.settings.save();
                            ui_components::apply_theme(ctx, true);
                        }

                        ui.separator();

                        // Group 1 (Left of divider): Independent Workspace Panel Toggles (Diff | AI | Terminal)
                        if ui_components::panel_toggle_button(
                            ui,
                            "Terminal",
                            self.show_terminal_panel,
                            accent,
                        )
                        .on_hover_text("เปิด/ปิดหน้าต่าง Terminal")
                        .clicked()
                        {
                            self.show_terminal_panel = !self.show_terminal_panel;
                            self.persist_ui_state();
                        }
                        if ui_components::panel_toggle_button(
                            ui,
                            "AI",
                            self.show_ai_panel,
                            accent,
                        )
                        .on_hover_text("เปิด/ปิดหน้าต่าง AI")
                        .clicked()
                        {
                            self.show_ai_panel = !self.show_ai_panel;
                            self.persist_ui_state();
                        }
                        if ui_components::panel_toggle_button(
                            ui,
                            "Diff",
                            self.show_diff_preview,
                            accent,
                        )
                        .on_hover_text("เปิด/ปิดหน้าต่าง Diff Preview")
                        .clicked()
                        {
                            self.show_diff_preview = !self.show_diff_preview;
                            self.persist_ui_state();
                        }
                    });
                });
            });

        // 2. Bottom Dock (only docked at bottom when Diff Preview is visible; otherwise fills CentralPanel)
        let show_any_bottom = self.show_ai_panel || self.show_terminal_panel;
        if self.show_diff_preview && show_any_bottom {
            let max_dock_h = (ctx.screen_rect().height() * 0.72).max(240.0);
            let dock_frame = Frame::new()
                .fill(canvas_bg)
                .stroke(Stroke::new(1.0_f32, t.border_hairline))
                .inner_margin(Margin::same(10));

            egui::TopBottomPanel::bottom("bottom_dock_v2")
                .frame(dock_frame)
                .resizable(true)
                .default_height(300.0)
                .height_range(200.0..=max_dock_h)
                .show(ctx, |ui| {
                    self.render_ai_and_terminal_area(ctx, ui, &t, accent);
                });
        }

        // 3. Diff Workspace (Left Sidebar + Central Diff Preview) — or Full-Height AI/Terminal when Diff is hidden
        if self.show_diff_preview {
        // Left Sidebar: Orca-Style Changed Files & Commit Graph
        let sidebar_frame = Frame::new()
            .fill(panel_bg)
            .stroke(Stroke::new(1.0, t.border_hairline))
            .inner_margin(Margin::same(12));

        egui::SidePanel::left("orca_files_sidebar")
            .frame(sidebar_frame)
            .resizable(true)
            .default_width(290.0)
            .min_width(220.0)
            .show(ctx, |ui| {
                // Top Row: Segmented Toggle (Changes | Graph) + Refresh
                ui.horizontal(|ui| {
                    ui_components::segmented_toggle(
                        ui,
                        &mut self.sidebar_tab,
                        &[
                            (SidebarTab::Changes, "Changes"),
                            (SidebarTab::Graph, "Graph"),
                        ],
                        accent,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_components::ghost_button(ui, "Refresh")
                            .on_hover_text("รีเฟรชรายการไฟล์ที่แก้ไขและเส้น Commit")
                            .clicked()
                        {
                            self.refresh_changes();
                        }
                    });
                });
                ui.add_space(6.0);

                match self.sidebar_tab {
                    SidebarTab::Changes => {
                        let total_add: usize = self.changes.iter().map(|c| c.added).sum();
                        let total_del: usize = self.changes.iter().map(|c| c.removed).sum();

                        ui.horizontal(|ui| {
                            ui_components::section_title(ui, "CHANGED FILES", t.accent_amber);
                            ui_components::pill(
                                ui,
                                &format!("{}", self.changes.len()),
                                t.text_primary,
                                t.bg_surface,
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("-{total_del}"))
                                            .size(11.5)
                                            .monospace()
                                            .color(t.accent_rose),
                                    );
                                    ui.label(
                                        RichText::new(format!("+{total_add}"))
                                            .size(11.5)
                                            .monospace()
                                            .color(t.accent_emerald),
                                    );
                                },
                            );
                        });
                        ui.add_space(8.0);

                        // Clean Filter Input
                        Frame::new()
                            .fill(t.bg_code)
                            .stroke(Stroke::new(1.0, t.border_hairline))
                            .corner_radius(CornerRadius::same(7))
                            .inner_margin(Margin::symmetric(9, 6))
                            .show(ui, |ui| {
                                ui.add_sized(
                                    [ui.available_width(), 18.0],
                                    TextEdit::singleline(&mut self.file_filter)
                                        .frame(false)
                                        .hint_text("Filter files..."),
                                );
                            });

                        ui.add_space(8.0);

                        egui::ScrollArea::vertical()
                            .id_salt("orca_file_list_scroll")
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                let filter_lower = self.file_filter.to_lowercase();
                                let mut shown = 0;

                                for (idx, change) in self.changes.iter().enumerate() {
                                    if !filter_lower.is_empty()
                                        && !change.path.to_lowercase().contains(&filter_lower)
                                    {
                                        continue;
                                    }
                                    shown += 1;

                                    let selected = idx == self.selected_change_idx;
                                    let bg = if selected {
                                        t.bg_surface
                                    } else {
                                        Color32::TRANSPARENT
                                    };
                                    let stroke = if selected {
                                        Stroke::new(1.0, accent)
                                    } else {
                                        Stroke::NONE
                                    };

                                    let (st_fg, _) =
                                        ui_components::status_colors(&t, &change.status);

                                    let resp = Frame::new()
                                        .fill(bg)
                                        .stroke(stroke)
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(7, 4))
                                        .show(ui, |ui| {
                                            ui.set_min_width(ui.available_width());
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new(&change.status)
                                                        .monospace()
                                                        .strong()
                                                        .size(11.5)
                                                        .color(st_fg),
                                                );
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&change.path)
                                                            .size(12.5)
                                                            .color(if selected {
                                                                t.text_primary
                                                            } else {
                                                                t.text_secondary
                                                            }),
                                                    )
                                                    .truncate(),
                                                );
                                            });
                                            ui.horizontal(|ui| {
                                                ui.add_space(2.0);
                                                ui.label(
                                                    RichText::new(format!("+{}", change.added))
                                                        .size(11.0)
                                                        .monospace()
                                                        .color(t.accent_emerald),
                                                );
                                                ui.label(
                                                    RichText::new(format!("-{}", change.removed))
                                                        .size(11.0)
                                                        .monospace()
                                                        .color(t.accent_rose),
                                                );
                                            });
                                        })
                                        .response
                                        .interact(egui::Sense::click());

                                    if resp.clicked() {
                                        self.selected_change_idx = idx;
                                    }
                                }

                                if shown == 0 {
                                    ui.add_space(24.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new("Working tree clean")
                                                .size(13.0)
                                                .color(t.accent_emerald),
                                        );
                                        ui.label(
                                            RichText::new("ยังไม่มีไฟล์ที่ถูกแก้ไข")
                                                .size(11.5)
                                                .color(t.text_muted),
                                        );
                                    });
                                }
                            });
                    }
                    SidebarTab::Graph => {
                        let commit_count =
                            self.commit_graph.iter().filter(|r| !r.hash.is_empty()).count();
                        ui.horizontal(|ui| {
                            ui_components::section_title(ui, "COMMIT GRAPH", t.accent_violet);
                            ui_components::pill(
                                ui,
                                &format!("{commit_count}"),
                                t.text_primary,
                                t.bg_surface,
                            );
                        });
                        ui.add_space(8.0);

                        // Filter commits by message / hash / author / branch ref
                        Frame::new()
                            .fill(t.bg_code)
                            .stroke(Stroke::new(1.0, t.border_hairline))
                            .corner_radius(CornerRadius::same(7))
                            .inner_margin(Margin::symmetric(9, 6))
                            .show(ui, |ui| {
                                ui.add_sized(
                                    [ui.available_width(), 18.0],
                                    TextEdit::singleline(&mut self.file_filter)
                                        .frame(false)
                                        .hint_text("Filter commits..."),
                                );
                            });

                        ui.add_space(8.0);

                        let mut clicked_commit: Option<String> = None;
                        egui::ScrollArea::vertical()
                            .id_salt("orca_commit_graph_scroll")
                            .show(ui, |ui| {
                                // Zero vertical spacing so graph lane lines connect cleanly across rows
                                ui.spacing_mut().item_spacing.y = 0.0;
                                let filter_lower = self.file_filter.to_lowercase();
                                let mut shown = 0;

                                for row in &self.commit_graph {
                                    // Pure graph connector row (e.g. `|\`, `|/`)
                                    if row.hash.is_empty() {
                                        if filter_lower.is_empty() {
                                            ui.horizontal(|ui| {
                                                ui.add_space(6.0);
                                                draw_commit_graph_lanes(ui, &t, &row.graph, 14.0);
                                            });
                                        }
                                        continue;
                                    }

                                    if !filter_lower.is_empty() {
                                        let matches = row
                                            .subject
                                            .to_lowercase()
                                            .contains(&filter_lower)
                                            || row.hash.to_lowercase().contains(&filter_lower)
                                            || row.author.to_lowercase().contains(&filter_lower)
                                            || row
                                                .refs
                                                .iter()
                                                .any(|r| r.to_lowercase().contains(&filter_lower));
                                        if !matches {
                                            continue;
                                        }
                                    }
                                    shown += 1;

                                    let selected = self.selected_commit_hash.as_deref()
                                        == Some(row.hash.as_str());
                                    let bg = if selected {
                                        t.bg_surface
                                    } else {
                                        Color32::TRANSPARENT
                                    };
                                    let stroke = if selected {
                                        Stroke::new(1.0, accent)
                                    } else {
                                        Stroke::NONE
                                    };

                                    let resp = Frame::new()
                                        .fill(bg)
                                        .stroke(stroke)
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(6, 0))
                                        .show(ui, |ui| {
                                            ui.set_min_width(ui.available_width());
                                            ui.horizontal(|ui| {
                                                draw_commit_graph_lanes(ui, &t, &row.graph, 42.0);
                                                ui.add_space(4.0);
                                                ui.vertical(|ui| {
                                                    ui.add_space(4.0);
                                                    // Line 1: Ref pills + Commit subject
                                                    ui.horizontal(|ui| {
                                                        if !row.refs.is_empty() {
                                                            for r in row.refs.iter().take(2) {
                                                                let r = r.trim();
                                                                if r.is_empty() {
                                                                    continue;
                                                                }
                                                                let (fg, rbg) = if r.contains("HEAD")
                                                                {
                                                                    (t.accent_emerald, t.diff_add_bg)
                                                                } else if r.starts_with("tag:") {
                                                                    (t.accent_amber, t.bg_code)
                                                                } else {
                                                                    (t.accent_violet, t.bg_code)
                                                                };
                                                                let short_ref =
                                                                    r.trim_start_matches("HEAD -> ");
                                                                ui_components::pill(
                                                                    ui, short_ref, fg, rbg,
                                                                );
                                                            }
                                                        }
                                                        ui.add(
                                                            egui::Label::new(
                                                                RichText::new(&row.subject)
                                                                    .size(12.0)
                                                                    .color(if selected {
                                                                        t.text_primary
                                                                    } else {
                                                                        t.text_secondary
                                                                    }),
                                                            )
                                                            .truncate(),
                                                        );
                                                    });
                                                    ui.add_space(2.0);
                                                    // Line 2: short hash · author · relative date
                                                    ui.horizontal(|ui| {
                                                        ui.label(
                                                            RichText::new(&row.hash)
                                                                .monospace()
                                                                .size(10.5)
                                                                .color(t.accent_indigo),
                                                        );
                                                        ui.add(
                                                            egui::Label::new(
                                                                RichText::new(format!(
                                                                    "{} · {}",
                                                                    row.author, row.date
                                                                ))
                                                                .size(10.5)
                                                                .color(t.text_muted),
                                                            )
                                                            .truncate(),
                                                        );
                                                    });
                                                    ui.add_space(4.0);
                                                });
                                            });
                                        })
                                        .response
                                        .interact(egui::Sense::click())
                                        .on_hover_cursor(egui::CursorIcon::PointingHand);

                                    if resp.clicked() {
                                        clicked_commit = Some(row.hash.clone());
                                    }
                                }

                                if shown == 0 {
                                    ui.add_space(24.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new("No commits found")
                                                .size(13.0)
                                                .color(t.text_secondary),
                                        );
                                        ui.label(
                                            RichText::new("ยังไม่มีประวัติ Git Commit ในโปรเจกต์นี้")
                                                .size(11.5)
                                                .color(t.text_muted),
                                        );
                                    });
                                }
                            });

                        if let Some(hash) = clicked_commit {
                            self.select_commit(hash);
                        }
                    }
                }
            });

        // 4. Center Main Area: Seamless Orca Diff Card (Header + Code Joined)
        let center_frame = Frame::new()
            .fill(canvas_bg)
            .inner_margin(Margin::same(12));

        let viewing_commit =
            self.sidebar_tab == SidebarTab::Graph && self.selected_commit_hash.is_some();

        egui::CentralPanel::default()
            .frame(center_frame)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(t.bg_code)
                    .stroke(Stroke::new(1.0, t.border_hairline))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(0))
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());

                        // Joined File Header Strip
                        Frame::new()
                            .fill(panel_bg)
                            .corner_radius(CornerRadius {
                                nw: 10,
                                ne: 10,
                                sw: 0,
                                se: 0,
                            })
                            .inner_margin(Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let active_change = if viewing_commit {
                                        self.selected_commit_changes
                                            .get(self.selected_commit_file_idx)
                                    } else {
                                        self.changes.get(self.selected_change_idx)
                                    };

                                    if viewing_commit {
                                        if let Some(hash) = &self.selected_commit_hash {
                                            ui_components::pill(
                                                ui,
                                                hash,
                                                t.accent_violet,
                                                t.bg_surface,
                                            );
                                        }
                                    }

                                    if let Some(change) = active_change {
                                        let (st_fg, _) =
                                            ui_components::status_colors(&t, &change.status);
                                        ui.label(
                                            RichText::new(&change.status)
                                                .monospace()
                                                .strong()
                                                .size(12.5)
                                                .color(st_fg),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&change.path)
                                                    .strong()
                                                    .monospace()
                                                    .size(13.5)
                                                    .color(t.text_primary),
                                            )
                                            .truncate(),
                                        );
                                        ui.add_space(6.0);
                                        ui_components::pill(
                                            ui,
                                            &format!("+{}", change.added),
                                            t.accent_emerald,
                                            t.diff_add_bg,
                                        );
                                        ui_components::pill(
                                            ui,
                                            &format!("-{}", change.removed),
                                            t.accent_rose,
                                            t.diff_del_bg,
                                        );
                                    } else {
                                        let title = if self.sidebar_tab == SidebarTab::Graph {
                                            "Commit Diff Preview"
                                        } else {
                                            "Diff Preview"
                                        };
                                        ui.label(
                                            RichText::new(title)
                                                .strong()
                                                .size(13.5)
                                                .color(t.text_secondary),
                                        );
                                    }

                                    // Unified Segmented Control (Vertical Y / Split X)
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui_components::segmented_toggle(
                                                ui,
                                                &mut self.diff_mode,
                                                &[
                                                    (DiffViewMode::Unified, "Vertical (Y)"),
                                                    (DiffViewMode::Split, "Split (X)"),
                                                ],
                                                accent,
                                            );
                                        },
                                    );
                                });
                            });

                        // If viewing a commit with multiple changed files, show a compact file selector bar
                        if viewing_commit && self.selected_commit_changes.len() > 1 {
                            ui.separator();
                            let mut next_file_idx: Option<usize> = None;
                            Frame::new()
                                .fill(panel_bg)
                                .inner_margin(Margin::symmetric(12, 6))
                                .show(ui, |ui| {
                                    egui::ScrollArea::horizontal()
                                        .id_salt("commit_files_strip")
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                for (i, ch) in
                                                    self.selected_commit_changes.iter().enumerate()
                                                {
                                                    let is_sel =
                                                        i == self.selected_commit_file_idx;
                                                    let (st_fg, _) = ui_components::status_colors(
                                                        &t,
                                                        &ch.status,
                                                    );
                                                    let short_name = ch
                                                        .path
                                                        .rsplit('/')
                                                        .next()
                                                        .unwrap_or(&ch.path);
                                                    let resp = Frame::new()
                                                        .fill(if is_sel {
                                                            t.bg_surface
                                                        } else {
                                                            Color32::TRANSPARENT
                                                        })
                                                        .stroke(if is_sel {
                                                            Stroke::new(1.0, accent)
                                                        } else {
                                                            Stroke::new(1.0, t.border_hairline)
                                                        })
                                                        .corner_radius(CornerRadius::same(6))
                                                        .inner_margin(Margin::symmetric(8, 3))
                                                        .show(ui, |ui| {
                                                            ui.horizontal(|ui| {
                                                                ui.label(
                                                                    RichText::new(&ch.status)
                                                                        .monospace()
                                                                        .strong()
                                                                        .size(11.0)
                                                                        .color(st_fg),
                                                                );
                                                                ui.label(
                                                                    RichText::new(short_name)
                                                                        .monospace()
                                                                        .size(11.5)
                                                                        .color(if is_sel {
                                                                            t.text_primary
                                                                        } else {
                                                                            t.text_secondary
                                                                        }),
                                                                );
                                                            });
                                                        })
                                                        .response
                                                        .interact(egui::Sense::click())
                                                        .on_hover_text(&ch.path)
                                                        .on_hover_cursor(
                                                            egui::CursorIcon::PointingHand,
                                                        );
                                                    if resp.clicked() {
                                                        next_file_idx = Some(i);
                                                    }
                                                }
                                            });
                                        });
                                });
                            if let Some(i) = next_file_idx {
                                self.selected_commit_file_idx = i;
                            }
                        }

                        ui.separator();

                        // Diff Code Body
                        let diff_scroll = if self.settings.word_wrap_diff {
                            egui::ScrollArea::vertical().id_salt("orca_diff_scroll_y")
                        } else {
                            egui::ScrollArea::both().id_salt("orca_diff_scroll_xy")
                        };
                        diff_scroll.show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.add_space(4.0);
                            let active_change = if viewing_commit {
                                self.selected_commit_changes
                                    .get(self.selected_commit_file_idx)
                            } else if self.sidebar_tab == SidebarTab::Graph {
                                None
                            } else {
                                self.changes.get(self.selected_change_idx)
                            };

                            if let Some(change) = active_change {
                                self.render_orca_diff(ui, change);
                            } else {
                                ui.add_space(60.0);
                                ui.vertical_centered(|ui| {
                                    let (heading, subtitle) = if viewing_commit {
                                        (
                                            "No file changes in this commit",
                                            "Commit นี้ไม่มีไฟล์ที่เปลี่ยนแปลง (เช่น merge commit หรือ empty commit)",
                                        )
                                    } else if self.sidebar_tab == SidebarTab::Graph {
                                        (
                                            "No commit selected",
                                            "เลือก commit จากแถบ COMMIT GRAPH ด้านซ้ายเพื่อดูรายละเอียด Diff",
                                        )
                                    } else {
                                        (
                                            "No file selected",
                                            "เลือกไฟล์จากแถบ CHANGED FILES ด้านซ้ายเพื่อดู Diff",
                                        )
                                    };
                                    ui.label(
                                        RichText::new(heading)
                                            .size(15.0)
                                            .strong()
                                            .color(t.text_secondary),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(subtitle)
                                            .size(12.5)
                                            .color(t.text_muted),
                                    );
                                });
                            }
                            ui.add_space(12.0);
                        });
                    });
            });
        } else {
            let full_frame = Frame::new()
                .fill(canvas_bg)
                .inner_margin(Margin::same(10));
            egui::CentralPanel::default()
                .frame(full_frame)
                .show(ctx, |ui| {
                    if show_any_bottom {
                        self.render_ai_and_terminal_area(ctx, ui, &t, accent);
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space((ui.available_height() * 0.35).max(40.0));
                                ui.label(
                                    RichText::new("ปิดหน้าต่างทั้งหมดอยู่")
                                        .size(16.0)
                                        .strong()
                                        .color(t.text_secondary),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("กดปุ่มด้านบนหรือเลือกเปิดหน้าต่างที่ต้องการด้านล่าง")
                                        .size(12.5)
                                        .color(t.text_muted),
                                );
                                ui.add_space(14.0);
                                ui.horizontal(|ui| {
                                    ui.add_space(((ui.available_width() - 250.0) * 0.5).max(0.0));
                                    if ui_components::subtle_button(ui, "เปิด Diff Preview")
                                        .clicked()
                                    {
                                        self.show_diff_preview = true;
                                    }
                                    if ui_components::subtle_button(ui, "เปิด AI").clicked() {
                                        self.show_ai_panel = true;
                                    }
                                    if ui_components::subtle_button(ui, "เปิด Terminal").clicked() {
                                        self.show_terminal_panel = true;
                                    }
                                });
                            });
                        });
                    }
                });
        }

        // 5. Modal Dialog: Create New Branch from Current Branch
        if self.show_new_branch_modal {
            let mut open = true;
            let mut create_now = false;
            let base = self
                .current_branch
                .clone()
                .unwrap_or_else(|| "HEAD".to_string());

            egui::Window::new("Create New Branch")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(390.0, 155.0))
                .frame(
                    Frame::new()
                        .fill(t.bg_panel)
                        .stroke(Stroke::new(1.0, t.border_subtle))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    // Custom theme-matched modal header
                    ui.horizontal(|ui| {
                        ui_components::icon_git_branch(ui, t.accent_violet, 16.0);
                        ui.label(
                            RichText::new("Create New Branch")
                                .strong()
                                .size(14.5)
                                .color(t.text_primary),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui_components::modal_close_button(ui).clicked() {
                                open = false;
                            }
                        });
                    });
                    ui.separator();
                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("แตก branch ใหม่จาก")
                                .size(12.5)
                                .color(t.text_secondary),
                        );
                        ui_components::pill(ui, &base, t.accent_violet, t.bg_surface);
                    });
                    ui.add_space(8.0);

                    Frame::new()
                        .fill(t.bg_code)
                        .stroke(Stroke::new(1.0, t.border_subtle))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui_components::icon_git_branch(ui, t.text_muted, 14.0);
                                let resp = ui.add_sized(
                                    [ui.available_width(), 20.0],
                                    TextEdit::singleline(&mut self.new_branch_input)
                                        .frame(false)
                                        .font(egui::TextStyle::Monospace)
                                        .hint_text("เช่น feature/login หรือ fix/ui"),
                                );
                                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    create_now = true;
                                }
                            });
                        });

                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let valid = !self.new_branch_input.trim().is_empty();
                        if ui_components::primary_button(ui, "Create Branch", accent, valid)
                            .clicked()
                        {
                            create_now = true;
                        }
                        if ui_components::ghost_button(ui, "Cancel").clicked() {
                            self.show_new_branch_modal = false;
                        }
                    });
                });

            if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.show_new_branch_modal = false;
            }
            if create_now {
                let name = self.new_branch_input.trim().to_string();
                self.show_new_branch_modal = false;
                self.new_branch_input.clear();
                self.checkout_branch(&name, true);
            }
        }

        // 6. Modal Dialog: Settings (General | AI & Agent | Terminal)
        if self.show_settings_modal {
            let mut open = true;
            let mut save_clicked = false;
            let mut reset_clicked = false;

            let screen = ctx.screen_rect();
            let modal_w = (screen.width() - 40.0).min(520.0).max(340.0);
            let modal_h = (screen.height() - 40.0).min(460.0).max(320.0);

            egui::Window::new("Settings")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(modal_w, modal_h))
                .frame(
                    Frame::new()
                        .fill(t.bg_panel)
                        .stroke(Stroke::new(1.0, t.border_subtle))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    // Custom theme-matched modal header
                    ui.horizontal(|ui| {
                        ui_components::icon_settings(ui, accent, 16.0);
                        ui.label(
                            RichText::new("Settings")
                                .strong()
                                .size(14.5)
                                .color(t.text_primary),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui_components::modal_close_button(ui).clicked() {
                                open = false;
                            }
                            ui.add_space(6.0);
                            ui_components::segmented_toggle(
                                ui,
                                &mut self.settings_tab,
                                &[
                                    (SettingsTab::General, "General"),
                                    (SettingsTab::Ai, "AI & Agent"),
                                    (SettingsTab::Terminal, "Terminal"),
                                ],
                                accent,
                            );
                        });
                    });
                    ui.separator();
                    ui.add_space(6.0);

                    match self.settings_tab {
                        SettingsTab::General => {
                            egui::Grid::new("settings_general_grid")
                                .num_columns(2)
                                .spacing([16.0, 12.0])
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("Theme").size(12.5).color(t.text_secondary),
                                    );
                                    ui.horizontal(|ui| {
                                        let mut is_dark = self.dark_mode;
                                        ui_components::segmented_toggle(
                                            ui,
                                            &mut is_dark,
                                            &[(true, "Dark"), (false, "Light")],
                                            accent,
                                        );
                                        if is_dark != self.dark_mode {
                                            self.dark_mode = is_dark;
                                            self.settings.dark_mode = is_dark;
                                            ui_components::apply_theme(ctx, is_dark);
                                        }
                                    });
                                    ui.end_row();

                                    ui.label(
                                        RichText::new("Font Family")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    let prev_font = self.settings.font_family.clone();
                                    let current_font_label = if self.settings.font_family.is_empty()
                                    {
                                        "Default".to_string()
                                    } else {
                                        self.settings.font_family.clone()
                                    };
                                    egui::ComboBox::from_id_salt("settings_font_family")
                                        .selected_text(&current_font_label)
                                        .width(240.0)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut self.settings.font_family,
                                                "Default".to_string(),
                                                "Default",
                                            );
                                            for (name, _) in &self.system_fonts {
                                                ui.selectable_value(
                                                    &mut self.settings.font_family,
                                                    name.clone(),
                                                    name,
                                                );
                                            }
                                        });
                                    if self.settings.font_family != prev_font {
                                        setup_fonts(
                                            ctx,
                                            &self.settings.font_family,
                                            &self.system_fonts,
                                        );
                                    }
                                    ui.end_row();

                                    ui.label(
                                        RichText::new("Diff Font Size")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    ui.add(
                                        egui::Slider::new(
                                            &mut self.settings.diff_font_size,
                                            10.0..=22.0,
                                        )
                                        .step_by(0.5)
                                        .suffix(" px"),
                                    );
                                    ui.end_row();

                                    ui.label(
                                        RichText::new("AI Font Size")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    ui.add(
                                        egui::Slider::new(
                                            &mut self.settings.ai_font_size,
                                            10.0..=22.0,
                                        )
                                        .step_by(0.5)
                                        .suffix(" px"),
                                    );
                                    ui.end_row();

                                    ui.label(
                                        RichText::new("Terminal Font Size")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    ui.add(
                                        egui::Slider::new(
                                            &mut self.settings.terminal_font_size,
                                            10.0..=22.0,
                                        )
                                        .step_by(0.5)
                                        .suffix(" px"),
                                    );
                                    ui.end_row();

                                    ui.label(
                                        RichText::new("Word Wrap in Diff")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    ui.checkbox(
                                        &mut self.settings.word_wrap_diff,
                                        "ตัดขึ้นบรรทัดใหม่อัตโนมัติ",
                                    );
                                    ui.end_row();

                                    ui.label(
                                        RichText::new("Auto-Refresh Diff")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut self.settings.auto_refresh_diff, "เปิดใช้งาน");
                                        if self.settings.auto_refresh_diff {
                                            egui::ComboBox::from_id_salt("settings_refresh_secs")
                                                .selected_text(format!(
                                                    "ทุก {}s",
                                                    self.settings.auto_refresh_secs
                                                ))
                                                .width(80.0)
                                                .show_ui(ui, |ui| {
                                                    for s in [1u64, 2, 5, 10] {
                                                        ui.selectable_value(
                                                            &mut self.settings.auto_refresh_secs,
                                                            s,
                                                            format!("ทุก {s}s"),
                                                        );
                                                    }
                                                });
                                        }
                                    });
                                    ui.end_row();
                                });
                        }
                        SettingsTab::Ai => {
                            egui::Grid::new("settings_ai_grid")
                                .num_columns(2)
                                .spacing([16.0, 12.0])
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("Execution Mode")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    egui::ComboBox::from_id_salt("settings_exec_mode")
                                        .selected_text(&self.settings.execution_mode)
                                        .width(180.0)
                                        .show_ui(ui, |ui| {
                                            for (val, label) in [
                                                ("auto", "auto (อัตโนมัติ)"),
                                                ("accept-edits", "accept-edits (อนุมัติการแก้ไฟล์)"),
                                                ("plan", "plan (วางแผนก่อนแก้)"),
                                            ] {
                                                ui.selectable_value(
                                                    &mut self.settings.execution_mode,
                                                    val.to_string(),
                                                    label,
                                                );
                                            }
                                        });
                                    ui.end_row();

                                    ui.label(
                                        RichText::new("Request Timeout")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    egui::ComboBox::from_id_salt("settings_timeout")
                                        .selected_text(if self.settings.request_timeout_secs == 0 {
                                            "No limit".to_string()
                                        } else {
                                            format!("{}s", self.settings.request_timeout_secs)
                                        })
                                        .width(120.0)
                                        .show_ui(ui, |ui| {
                                            for (secs, label) in [
                                                (0u64, "No limit"),
                                                (30, "30s"),
                                                (60, "60s"),
                                                (120, "120s"),
                                            ] {
                                                ui.selectable_value(
                                                    &mut self.settings.request_timeout_secs,
                                                    secs,
                                                    label,
                                                );
                                            }
                                        });
                                    ui.end_row();
                                });

                            ui.add_space(10.0);
                            ui.label(
                                RichText::new("Custom System Instruction")
                                    .size(12.0)
                                    .color(t.text_secondary),
                            );
                            ui.add_space(4.0);
                            Frame::new()
                                .fill(t.bg_code)
                                .stroke(Stroke::new(1.0, t.border_subtle))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::same(8))
                                .show(ui, |ui| {
                                    ui.add_sized(
                                        [ui.available_width(), 64.0],
                                        TextEdit::multiline(&mut self.settings.system_instruction)
                                            .frame(false)
                                            .hint_text("เช่น ตอบเป็นภาษาไทยแบบกระชับ และอธิบายโค้ดทุกครั้ง..."),
                                    );
                                });
                        }
                        SettingsTab::Terminal => {
                            egui::Grid::new("settings_term_grid")
                                .num_columns(2)
                                .spacing([16.0, 12.0])
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("Default Shell")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    egui::ComboBox::from_id_salt("settings_shell")
                                        .selected_text(&self.settings.shell)
                                        .width(140.0)
                                        .show_ui(ui, |ui| {
                                            for sh in ["zsh", "bash", "sh"] {
                                                ui.selectable_value(
                                                    &mut self.settings.shell,
                                                    sh.to_string(),
                                                    sh,
                                                );
                                            }
                                        });
                                    ui.end_row();

                                    ui.label(
                                        RichText::new("Max Scrollback")
                                            .size(12.5)
                                            .color(t.text_secondary),
                                    );
                                    egui::ComboBox::from_id_salt("settings_max_lines")
                                        .selected_text(format!(
                                            "{} lines",
                                            self.settings.max_terminal_lines
                                        ))
                                        .width(140.0)
                                        .show_ui(ui, |ui| {
                                            for n in [200usize, 500, 1000, 2000] {
                                                ui.selectable_value(
                                                    &mut self.settings.max_terminal_lines,
                                                    n,
                                                    format!("{n} lines"),
                                                );
                                            }
                                        });
                                    ui.end_row();
                                });
                        }
                    }

                    // Footer bar pinned at bottom of Settings modal
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        ui.horizontal(|ui| {
                            if let Some(status) = &self.settings_status {
                                ui.label(
                                    RichText::new(status)
                                        .size(11.5)
                                        .color(t.accent_emerald),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui_components::primary_button(ui, "Save", accent, true)
                                        .clicked()
                                    {
                                        save_clicked = true;
                                    }
                                    if ui_components::ghost_button(ui, "Reset Defaults").clicked()
                                    {
                                        reset_clicked = true;
                                    }
                                },
                            );
                        });
                        ui.separator();
                    });
                });

            if reset_clicked {
                self.settings = AppSettings::default();
                self.dark_mode = self.settings.dark_mode;
                self.show_diff_preview = self.settings.show_diff_preview;
                self.show_ai_panel = self.settings.show_ai_panel;
                self.show_terminal_panel = self.settings.show_terminal_panel;
                ui_components::apply_theme(ctx, self.dark_mode);
                setup_fonts(ctx, &self.settings.font_family, &self.system_fonts);
                let _ = self.settings.save();
                self.settings_status = Some("รีเซ็ตเป็นค่าเริ่มต้นและบันทึกแล้ว".into());
            }
            if save_clicked {
                self.settings.dark_mode = self.dark_mode;
                self.settings.selected_provider_id = self.selected_provider_id.clone();
                self.settings.selected_base_model = self.selected_base_model.clone();
                self.settings.selected_level = self.selected_level.clone();
                self.settings.selected_agent = self.selected_agent.clone();
                self.settings.show_diff_preview = self.show_diff_preview;
                self.settings.show_ai_panel = self.show_ai_panel;
                self.settings.show_terminal_panel = self.show_terminal_panel;
                match self.settings.save() {
                    Ok(path) => {
                        self.settings_status =
                            Some(format!("บันทึกแล้วที่ {}", path.display()));
                        self.show_settings_modal = false;
                    }
                    Err(e) => {
                        self.settings_status = Some(format!("บันทึกไม่สำเร็จ: {e}"));
                    }
                }
            } else if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.settings.dark_mode = self.dark_mode;
                self.persist_ui_state();
                self.show_settings_modal = false;
            }
        }
    }
}
