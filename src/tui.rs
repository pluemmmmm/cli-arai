use crate::ai::{self, AiProvider};
use crate::changes::{ChangeTracker, DiffTag, FileChange};
use crate::rune_engine;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivePane {
    Prompt,
    Changes,
    Terminal,
}

#[derive(Debug, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

enum BgEvent {
    AiReply { provider: String, text: String },
    AiError(String),
    RuneResult(String),
    TerminalOutput { cmd: String, output: String, ok: bool },
    ProvidersRefreshed(Vec<AiProvider>),
}

struct App {
    cwd: PathBuf,
    active_pane: ActivePane,
    providers: Vec<AiProvider>,
    selected_provider_id: Option<String>,
    tracker: ChangeTracker,
    changes: Vec<FileChange>,
    selected_change_idx: usize,

    // Left pane: AI Prompt & Chat
    chat: Vec<ChatMessage>,
    prompt_input: String,
    chat_scroll: u16,
    ai_busy: bool,

    // Bottom-right pane: Separate Terminal
    term_lines: Vec<(String, Color)>,
    term_input: String,
    term_scroll: u16,
    term_busy: bool,

    // Background task channel
    tx: mpsc::UnboundedSender<BgEvent>,
    rx: mpsc::UnboundedReceiver<BgEvent>,
}

impl App {
    fn new(cwd: PathBuf, providers: Vec<AiProvider>, tracker: ChangeTracker) -> Self {
        let selected_provider_id = ai::select_provider(&providers, None).map(|p| p.id.clone());
        let changes = tracker.snapshot(&cwd);
        let (tx, rx) = mpsc::unbounded_channel();

        let available_list: Vec<String> = providers
            .iter()
            .filter(|p| p.available)
            .map(|p| format!("{} ({})", p.id, p.name))
            .collect();

        let welcome = if available_list.is_empty() {
            "ตรวจไม่พบ AI CLI หรือ API Key ที่พร้อมใช้งาน (ตั้งค่า OPENAI_API_KEY / ANTHROPIC_API_KEY / GEMINI_API_KEY หรือรัน Ollama)\nพิมพ์ /ai เพื่อดูรายการที่ตรวจสอบทั้งหมด หรือ /rune <code> เพื่อรันสคริปต์ Rune ได้ทันที".to_string()
        } else {
            format!(
                "ตรวจพบ AI ที่พร้อมใช้งาน: {}\nพิมพ์พร้อมพ์เพื่อคุยกับ AI, /use <id> เพื่อสลับ AI, /rune <code> เพื่อรัน Rune, หรือกด Tab เพื่อสลับไป Terminal แยก",
                available_list.join(", ")
            )
        };

        Self {
            cwd,
            active_pane: ActivePane::Prompt,
            providers,
            selected_provider_id,
            tracker,
            changes,
            selected_change_idx: 0,
            chat: vec![ChatMessage {
                role: "system".to_string(),
                content: welcome,
            }],
            prompt_input: String::new(),
            chat_scroll: 0,
            ai_busy: false,
            term_lines: vec![
                (
                    "Separate Terminal Ready — กด Tab เพื่อโฟกัสช่องนี้ แล้วพิมพ์คำสั่ง shell ได้เลย".to_string(),
                    Color::DarkGray,
                ),
            ],
            term_input: String::new(),
            term_scroll: 0,
            term_busy: false,
            tx,
            rx,
        }
    }

    fn active_provider(&self) -> Option<&AiProvider> {
        ai::select_provider(&self.providers, self.selected_provider_id.as_deref())
    }

    fn refresh_changes(&mut self) {
        self.changes = self.tracker.snapshot(&self.cwd);
        if self.selected_change_idx >= self.changes.len() && !self.changes.is_empty() {
            self.selected_change_idx = self.changes.len() - 1;
        }
    }

    fn handle_bg_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                BgEvent::AiReply { provider, text } => {
                    self.ai_busy = false;
                    self.chat.push(ChatMessage {
                        role: provider,
                        content: text,
                    });
                    self.refresh_changes();
                }
                BgEvent::AiError(err) => {
                    self.ai_busy = false;
                    self.chat.push(ChatMessage {
                        role: "error".to_string(),
                        content: err,
                    });
                }
                BgEvent::RuneResult(out) => {
                    self.ai_busy = false;
                    self.chat.push(ChatMessage {
                        role: "rune".to_string(),
                        content: out,
                    });
                    self.refresh_changes();
                }
                BgEvent::TerminalOutput { cmd, output, ok } => {
                    self.term_busy = false;
                    self.term_lines.push((format!("$ {cmd}"), Color::Cyan));
                    let color = if ok { Color::White } else { Color::LightRed };
                    for line in output.lines() {
                        self.term_lines.push((line.to_string(), color));
                    }
                    if output.is_empty() {
                        self.term_lines.push(("(no output)".to_string(), Color::DarkGray));
                    }
                    self.refresh_changes();
                }
                BgEvent::ProvidersRefreshed(list) => {
                    self.providers = list;
                    if self.active_provider().is_none() {
                        self.selected_provider_id =
                            ai::select_provider(&self.providers, None).map(|p| p.id.clone());
                    }
                }
            }
        }
    }

    fn submit_prompt(&mut self) {
        let input = self.prompt_input.trim().to_string();
        if input.is_empty() {
            return;
        }
        self.prompt_input.clear();

        // Slash commands inside the prompt box
        if input == "/help" {
            self.chat.push(ChatMessage {
                role: "system".into(),
                content: "คำสั่งที่ใช้ได้:\n  /ai          — ตรวจสอบ AI ที่มีอยู่ในเครื่องทั้งหมด\n  /use <id>    — เปลี่ยน AI provider\n  /rune <code> — รันโค้ด Rune แบบ inline (รองรับ async/await)\n  /run <file>  — รันไฟล์สคริปต์ .rn\n  /diff        — รีเฟรชหน้าต่างแสดงการแก้ไขไฟล์\n  /clear       — ล้างประวัติแชท\nคีย์ลัด:\n  Tab          — สลับโฟกัส (AI Prompt -> Changes -> Terminal)\n  Ctrl+R       — รีเฟรช Changes/Diff\n  Ctrl+C / Esc — ออกจากโปรแกรม".into(),
            });
            return;
        }

        if input == "/clear" {
            self.chat.clear();
            return;
        }

        if input == "/diff" {
            self.refresh_changes();
            self.chat.push(ChatMessage {
                role: "system".into(),
                content: format!("รีเฟรชรายการแก้ไขแล้ว (พบ {} ไฟล์)", self.changes.len()),
            });
            return;
        }

        if input == "/ai" {
            let mut lines = vec!["รายการ AI ที่ตรวจสอบในเครื่อง:".to_string()];
            for p in &self.providers {
                let mark = if p.available { "✅" } else { "⬜" };
                lines.push(format!("  {mark} {:<12} {} — {}", p.id, p.name, p.detail));
            }
            self.chat.push(ChatMessage {
                role: "system".into(),
                content: lines.join("\n"),
            });
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let refreshed = ai::detect_providers().await;
                let _ = tx.send(BgEvent::ProvidersRefreshed(refreshed));
            });
            return;
        }

        if let Some(id) = input.strip_prefix("/use ") {
            let id = id.trim();
            if self.providers.iter().any(|p| p.id == id && p.available) {
                self.selected_provider_id = Some(id.to_string());
                self.chat.push(ChatMessage {
                    role: "system".into(),
                    content: format!("สลับไปใช้ AI provider: {id}"),
                });
            } else {
                self.chat.push(ChatMessage {
                    role: "error".into(),
                    content: format!("ไม่พบ provider `{id}` ที่พร้อมใช้งาน (พิมพ์ /ai เพื่อดูรายชื่อ)"),
                });
            }
            return;
        }

        if let Some(code) = input.strip_prefix("/rune ") {
            let code = code.trim().to_string();
            self.chat.push(ChatMessage {
                role: "you".into(),
                content: format!("/rune {code}"),
            });
            self.ai_busy = true;
            let tx = self.tx.clone();
            tokio::spawn(async move {
                match rune_engine::eval_inline(&code).await {
                    Ok(out) => {
                        let _ = tx.send(BgEvent::RuneResult(out));
                    }
                    Err(e) => {
                        let _ = tx.send(BgEvent::AiError(format!("Rune error: {e}")));
                    }
                }
            });
            return;
        }

        if let Some(file) = input.strip_prefix("/run ") {
            let path = self.cwd.join(file.trim());
            self.chat.push(ChatMessage {
                role: "you".into(),
                content: format!("/run {}", path.display()),
            });
            self.ai_busy = true;
            let tx = self.tx.clone();
            tokio::spawn(async move {
                match rune_engine::run_file(&path).await {
                    Ok(out) => {
                        let _ = tx.send(BgEvent::RuneResult(out));
                    }
                    Err(e) => {
                        let _ = tx.send(BgEvent::AiError(format!("Rune file error: {e}")));
                    }
                }
            });
            return;
        }

        // Normal AI prompt
        self.chat.push(ChatMessage {
            role: "you".into(),
            content: input.clone(),
        });

        let Some(provider) = self.active_provider().cloned() else {
            self.chat.push(ChatMessage {
                role: "error".into(),
                content: "ยังไม่มี AI provider ที่พร้อมใช้งาน — พิมพ์ /ai เพื่อดูวิธีตั้งค่า".into(),
            });
            return;
        };

        self.ai_busy = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match ai::ask_provider(&provider, &input).await {
                Ok(reply) => {
                    let _ = tx.send(BgEvent::AiReply {
                        provider: provider.name,
                        text: reply,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::AiError(format!("AI error: {e}")));
                }
            }
        });
    }

    fn submit_terminal(&mut self) {
        let cmd = self.term_input.trim().to_string();
        if cmd.is_empty() {
            return;
        }
        self.term_input.clear();

        if cmd == "clear" {
            self.term_lines.clear();
            return;
        }

        self.term_busy = true;
        let cwd = self.cwd.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&cwd)
                .output()
                .await;

            match result {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let combined = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
                        (false, false) => format!("{}\n{}", stdout.trim(), stderr.trim()),
                        (false, true) => stdout.trim().to_string(),
                        (true, false) => stderr.trim().to_string(),
                        (true, true) => String::new(),
                    };
                    let _ = tx.send(BgEvent::TerminalOutput {
                        cmd,
                        output: combined,
                        ok: out.status.success(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::TerminalOutput {
                        cmd,
                        output: format!("spawn error: {e}"),
                        ok: false,
                    });
                }
            }
        });
    }
}

pub async fn run_tui(tracker: ChangeTracker) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let providers = ai::detect_providers().await;
    let mut app = App::new(cwd, providers, tracker);

    let mut terminal = ratatui::init();
    let res = run_event_loop(&mut terminal, &mut app);
    ratatui::restore();
    res
}

fn run_event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        app.handle_bg_events();
        terminal.draw(|f| draw_ui(f, app))?;

        if event::poll(Duration::from_millis(60))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Global shortcuts
                if key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    return Ok(());
                }

                if key.code == KeyCode::Tab {
                    app.active_pane = match app.active_pane {
                        ActivePane::Prompt => ActivePane::Changes,
                        ActivePane::Changes => ActivePane::Terminal,
                        ActivePane::Terminal => ActivePane::Prompt,
                    };
                    continue;
                }

                if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    app.refresh_changes();
                    continue;
                }

                if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    app.active_pane = ActivePane::Terminal;
                    continue;
                }

                match app.active_pane {
                    ActivePane::Prompt => match key.code {
                        KeyCode::Enter => app.submit_prompt(),
                        KeyCode::Backspace => {
                            app.prompt_input.pop();
                        }
                        KeyCode::Up => {
                            app.chat_scroll = app.chat_scroll.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            app.chat_scroll = app.chat_scroll.saturating_add(1);
                        }
                        KeyCode::Char(c) => app.prompt_input.push(c),
                        _ => {}
                    },
                    ActivePane::Changes => match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.selected_change_idx = app.selected_change_idx.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if !app.changes.is_empty() {
                                app.selected_change_idx =
                                    (app.selected_change_idx + 1).min(app.changes.len() - 1);
                            }
                        }
                        KeyCode::Char('r') => app.refresh_changes(),
                        _ => {}
                    },
                    ActivePane::Terminal => match key.code {
                        KeyCode::Enter => app.submit_terminal(),
                        KeyCode::Backspace => {
                            app.term_input.pop();
                        }
                        KeyCode::Up => {
                            app.term_scroll = app.term_scroll.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            app.term_scroll = app.term_scroll.saturating_add(1);
                        }
                        KeyCode::Char(c) => app.term_input.push(c),
                        _ => {}
                    },
                }
            }
        }
    }
}

fn draw_ui(f: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Main split
            Constraint::Length(1), // Footer help bar
        ])
        .split(f.area());

    draw_header(f, app, root[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(root[1]);

    draw_ai_pane(f, app, body[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(body[1]);

    draw_changes_pane(f, app, right[0]);
    draw_terminal_pane(f, app, right[1]);

    let footer = Paragraph::new(
        " Tab: สลับหน้าต่าง | Ctrl+T: โฟกัส Terminal | Ctrl+R: รีเฟรช Diff | /help: คำสั่ง | Esc/Ctrl+C: ออก ",
    )
    .style(Style::default().fg(Color::Black).bg(Color::Cyan));
    f.render_widget(footer, root[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let avail_count = app.providers.iter().filter(|p| p.available).count();
    let active_name = app
        .active_provider()
        .map(|p| format!("{} [{}]", p.name, p.id))
        .unwrap_or_else(|| "None (run /ai)".to_string());

    let title = Line::from(vec![
        Span::styled(
            " cli arai (Rust + Rune) ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | AI: "),
        Span::styled(
            active_name,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" ({avail_count} detected)")),
        Span::raw(format!(" | Dir: {}", app.cwd.display())),
    ]);

    let block = Block::default().borders(Borders::ALL);
    f.render_widget(Paragraph::new(title).block(block), area);
}

fn draw_ai_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.active_pane == ActivePane::Prompt;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(area);

    let mut lines = Vec::new();
    for msg in &app.chat {
        let role_color = match msg.role.as_str() {
            "you" => Color::Yellow,
            "rune" => Color::Magenta,
            "error" => Color::LightRed,
            "system" => Color::Cyan,
            _ => Color::Green,
        };
        lines.push(Line::from(Span::styled(
            format!("[{}]", msg.role),
            Style::default()
                .fg(role_color)
                .add_modifier(Modifier::BOLD),
        )));
        for l in msg.content.lines() {
            lines.push(Line::from(format!("  {l}")));
        }
        lines.push(Line::from(""));
    }

    if app.ai_busy {
        lines.push(Line::from(Span::styled(
            "⏳ กำลังประมวลผล...",
            Style::default().fg(Color::Yellow),
        )));
    }

    let chat_widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.chat_scroll, 0))
        .block(
            Block::default()
                .title(" AI & Rune Console ")
                .borders(Borders::ALL)
                .border_style(border_style),
        );
    f.render_widget(chat_widget, chunks[0]);

    let prompt_text = format!("> {}", app.prompt_input);
    let input_widget = Paragraph::new(prompt_text).block(
        Block::default()
            .title(" Prompt (Enter ส่ง | /ai | /rune <code>) ")
            .borders(Borders::ALL)
            .border_style(border_style),
    );
    f.render_widget(input_widget, chunks[1]);
}

fn draw_changes_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.active_pane == ActivePane::Changes;
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(4)])
        .split(area);

    // Top: changed file list
    let items: Vec<ListItem> = if app.changes.is_empty() {
        vec![ListItem::new(Span::styled(
            "  ไม่มีไฟล์ที่ถูกแก้ไข (working tree clean)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.changes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let marker = if i == app.selected_change_idx { "▸" } else { " " };
                let line = Line::from(vec![
                    Span::raw(format!("{marker} [{}] {} ", c.status, c.path)),
                    Span::styled(format!("+{}", c.added), Style::default().fg(Color::Green)),
                    Span::raw(" "),
                    Span::styled(format!("-{}", c.removed), Style::default().fg(Color::Red)),
                ]);
                ListItem::new(line)
            })
            .collect()
    };

    let list_widget = List::new(items).block(
        Block::default()
            .title(format!(" Changes ({}) ", app.changes.len()))
            .borders(Borders::ALL)
            .border_style(border_style),
    );
    f.render_widget(list_widget, split[0]);

    // Bottom: Diff preview of selected file
    let diff_lines: Vec<Line> = if let Some(change) = app.changes.get(app.selected_change_idx) {
        change
            .diff_lines
            .iter()
            .map(|dl| {
                let style = match dl.tag {
                    DiffTag::Header => Style::default().fg(Color::Cyan),
                    DiffTag::Added => Style::default().fg(Color::Green),
                    DiffTag::Removed => Style::default().fg(Color::Red),
                    DiffTag::Context => Style::default().fg(Color::Gray),
                };
                Line::from(Span::styled(dl.content.clone(), style))
            })
            .collect()
    } else {
        vec![Line::from(Span::styled(
            "  เลือกไฟล์เพื่อดู Diff (หรือใช้ Rune `cli::write_file` / แก้ไฟล์ในโปรเจกต์)",
            Style::default().fg(Color::DarkGray),
        ))]
    };

    let diff_widget = Paragraph::new(diff_lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(" Diff Preview ")
                .borders(Borders::ALL)
                .border_style(border_style),
        );
    f.render_widget(diff_widget, split[1]);
}

fn draw_terminal_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.active_pane == ActivePane::Terminal;
    let border_style = if focused {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    let mut lines: Vec<Line> = app
        .term_lines
        .iter()
        .map(|(text, color)| Line::from(Span::styled(text.clone(), Style::default().fg(*color))))
        .collect();

    if app.term_busy {
        lines.push(Line::from(Span::styled(
            "⏳ running...",
            Style::default().fg(Color::Yellow),
        )));
    }

    let out_widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.term_scroll, 0))
        .block(
            Block::default()
                .title(" Separate Terminal ")
                .borders(Borders::ALL)
                .border_style(border_style),
        );
    f.render_widget(out_widget, chunks[0]);

    let cmd_widget = Paragraph::new(format!("$ {}", app.term_input)).block(
        Block::default()
            .title(" Shell Input (Enter รันคำสั่ง) ")
            .borders(Borders::ALL)
            .border_style(border_style),
    );
    f.render_widget(cmd_widget, chunks[1]);
}
