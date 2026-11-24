use crate::ui_components::ThemeColors;
use eframe::egui::{self, Color32, CornerRadius, Event, FontId, Key, Pos2, Rect, Stroke, Vec2};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct PtyTerminal {
    parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    rows: u16,
    cols: u16,
    scrollback_offset: usize,
    pub exited: Arc<Mutex<bool>>,
}

impl PtyTerminal {
    pub fn spawn(cwd: &Path, ctx: egui::Context) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let initial_rows = 24;
        let initial_cols = 80;
        let pair = pty_system.openpty(PtySize {
            rows: initial_rows,
            cols: initial_cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let shell = if cfg!(windows) {
            "powershell.exe".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
        };
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if std::env::var("LANG").is_err() {
            cmd.env("LANG", "en_US.UTF-8");
        }
        if shell.ends_with("zsh") {
            if let Some(zdotdir) = prepare_clean_zdotdir() {
                cmd.env("ZDOTDIR", zdotdir);
            }
        }

        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            initial_rows,
            initial_cols,
            2000,
        )));
        let exited = Arc::new(Mutex::new(false));

        let parser_bg = Arc::clone(&parser);
        let exited_bg = Arc::clone(&exited);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        if let Ok(mut ex) = exited_bg.lock() {
                            *ex = true;
                        }
                        ctx.request_repaint();
                        break;
                    }
                    Ok(n) => {
                        if let Ok(mut p) = parser_bg.lock() {
                            p.process(&buf[..n]);
                        }
                        ctx.request_repaint();
                    }
                    Err(_) => {
                        if let Ok(mut ex) = exited_bg.lock() {
                            *ex = true;
                        }
                        ctx.request_repaint();
                        break;
                    }
                }
            }
        });

        Ok(Self {
            parser,
            master: pair.master,
            writer,
            _child: child,
            rows: initial_rows,
            cols: initial_cols,
            scrollback_offset: 0,
            exited,
        })
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(4);
        let cols = cols.max(16);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    pub fn write_bytes(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // Reset scrollback to bottom on user input (just like VS Code)
        if self.scrollback_offset > 0 {
            self.scrollback_offset = 0;
            if let Ok(mut p) = self.parser.lock() {
                p.set_scrollback(0);
            }
        }
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn clear(&mut self) {
        // Send Ctrl+L so the shell cleanly clears & redraws the prompt
        self.write_bytes(&[0x0c]);
    }

    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        t: &ThemeColors,
        accent: Color32,
        id_salt: &str,
        font_size: f32,
    ) {
        let font_id = FontId::monospace(font_size);
        let (char_w, line_h) = ui.fonts(|f| {
            let w = f.glyph_width(&font_id, 'M').max(6.0);
            let h = f.row_height(&font_id).max(14.0) + 1.0;
            (w, h)
        });

        let avail = ui.available_size();
        let cols = ((avail.x - 4.0) / char_w).floor().max(16.0) as u16;
        let rows = ((avail.y - 4.0) / line_h).floor().max(4.0) as u16;
        self.resize(rows, cols);

        let (rect, _) = ui.allocate_exact_size(avail.max(Vec2::new(60.0, 40.0)), egui::Sense::hover());
        let term_id = ui.make_persistent_id(id_salt);
        let resp = ui.interact(rect, term_id, egui::Sense::click_and_drag());

        if resp.clicked()
            || resp.drag_started()
            || (resp.hovered() && ui.input(|i| i.pointer.primary_pressed()))
        {
            resp.request_focus();
        }

        let has_focus = resp.has_focus();

        // Mouse wheel scrollback when hovering over terminal
        if resp.hovered() {
            let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll_y.abs() > 0.5 {
                let delta_lines = (scroll_y / line_h).round() as isize;
                if delta_lines != 0 {
                    let next = (self.scrollback_offset as isize + delta_lines).max(0) as usize;
                    if let Ok(mut p) = self.parser.lock() {
                        p.set_scrollback(next);
                        self.scrollback_offset = p.screen().scrollback();
                    }
                }
            }
        }

        // Forward keyboard events to PTY when focused
        if has_focus {
            // Prevent egui from stealing Tab focus away from the terminal
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    term_id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });

            let app_cursor = self
                .parser
                .lock()
                .map(|p| p.screen().application_cursor())
                .unwrap_or(false);

            let events = ui.input(|i| i.events.clone());
            for ev in events {
                match ev {
                    Event::Text(text) => {
                        self.write_bytes(text.as_bytes());
                    }
                    Event::Paste(text) => {
                        self.write_bytes(text.as_bytes());
                    }
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        // Ctrl + A..Z -> ASCII control codes 0x01..0x1A
                        if modifiers.ctrl && !modifiers.command {
                            if let Some(ctrl_byte) = key_to_ctrl_byte(key) {
                                self.write_bytes(&[ctrl_byte]);
                                continue;
                            }
                        }
                        // Cmd+K on macOS clears terminal (like VS Code / iTerm2)
                        if modifiers.command && key == Key::K {
                            self.clear();
                            continue;
                        }
                        if modifiers.command {
                            continue;
                        }
                        let seq: Option<&[u8]> = match key {
                            Key::Enter => Some(b"\r"),
                            Key::Backspace => Some(b"\x7f"),
                            Key::Tab => {
                                if modifiers.shift {
                                    Some(b"\x1b[Z")
                                } else {
                                    Some(b"\t")
                                }
                            }
                            Key::Escape => Some(b"\x1b"),
                            Key::ArrowUp => Some(if app_cursor { b"\x1bOA" } else { b"\x1b[A" }),
                            Key::ArrowDown => Some(if app_cursor { b"\x1bOB" } else { b"\x1b[B" }),
                            Key::ArrowRight => Some(if app_cursor { b"\x1bOC" } else { b"\x1b[C" }),
                            Key::ArrowLeft => Some(if app_cursor { b"\x1bOD" } else { b"\x1b[D" }),
                            Key::Home => Some(b"\x1b[H"),
                            Key::End => Some(b"\x1b[F"),
                            Key::PageUp => Some(b"\x1b[5~"),
                            Key::PageDown => Some(b"\x1b[6~"),
                            Key::Delete => Some(b"\x1b[3~"),
                            Key::Insert => Some(b"\x1b[2~"),
                            _ => None,
                        };
                        if let Some(bytes) = seq {
                            self.write_bytes(bytes);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Paint terminal grid
        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, CornerRadius::ZERO, t.bg_code);

            let origin = Pos2::new(rect.min.x + 2.0, rect.min.y + 2.0);
            let default_fg = t.text_primary;
            let default_bg = t.bg_code;

            if let Ok(parser) = self.parser.lock() {
                let screen = parser.screen();
                let (s_rows, s_cols) = screen.size();

                for r in 0..s_rows {
                    let y = origin.y + (r as f32) * line_h;
                    let mut c = 0u16;
                    while c < s_cols {
                        let Some(cell) = screen.cell(r, c) else {
                            c += 1;
                            continue;
                        };
                        if cell.is_wide_continuation() {
                            c += 1;
                            continue;
                        }

                        let (mut fg, mut bg) = (
                            vt_color_to_egui(cell.fgcolor(), default_fg, t),
                            vt_color_to_egui(cell.bgcolor(), default_bg, t),
                        );
                        if cell.inverse() {
                            std::mem::swap(&mut fg, &mut bg);
                        }
                        let underline = cell.underline();

                        // Group consecutive cells with identical style into a single run
                        let start_c = c;
                        let mut run = String::new();
                        while c < s_cols {
                            let Some(cur) = screen.cell(r, c) else {
                                break;
                            };
                            if cur.is_wide_continuation() {
                                c += 1;
                                continue;
                            }
                            let (mut c_fg, mut c_bg) = (
                                vt_color_to_egui(cur.fgcolor(), default_fg, t),
                                vt_color_to_egui(cur.bgcolor(), default_bg, t),
                            );
                            if cur.inverse() {
                                std::mem::swap(&mut c_fg, &mut c_bg);
                            }
                            if c_fg != fg || c_bg != bg || cur.underline() != underline {
                                break;
                            }
                            let ch = cur.contents();
                            if ch.is_empty() {
                                run.push(' ');
                            } else {
                                run.push_str(&ch);
                            }
                            c += if cur.is_wide() { 2 } else { 1 };
                        }

                        let run_x = origin.x + (start_c as f32) * char_w;
                        let run_w = ((c - start_c) as f32) * char_w;

                        if bg != default_bg {
                            painter.rect_filled(
                                Rect::from_min_size(Pos2::new(run_x, y), Vec2::new(run_w, line_h)),
                                CornerRadius::ZERO,
                                bg,
                            );
                        }

                        if !run.trim_end().is_empty() {
                            painter.text(
                                Pos2::new(run_x, y),
                                egui::Align2::LEFT_TOP,
                                &run,
                                font_id.clone(),
                                fg,
                            );
                        }

                        if underline {
                            let uy = y + line_h - 1.5;
                            painter.line_segment(
                                [Pos2::new(run_x, uy), Pos2::new(run_x + run_w, uy)],
                                Stroke::new(1.0_f32, fg),
                            );
                        }
                    }
                }

                // Draw terminal cursor (only when not scrolled up into history)
                if !screen.hide_cursor() && self.scrollback_offset == 0 {
                    let (cur_r, cur_c) = screen.cursor_position();
                    let cx = origin.x + (cur_c as f32) * char_w;
                    let cy = origin.y + (cur_r as f32) * line_h;
                    let cursor_rect =
                        Rect::from_min_size(Pos2::new(cx, cy), Vec2::new(char_w.max(7.0), line_h));
                    if has_focus {
                        painter.rect_filled(
                            cursor_rect,
                            CornerRadius::same(1),
                            accent.gamma_multiply(0.75),
                        );
                    } else {
                        painter.rect_stroke(
                            cursor_rect,
                            CornerRadius::same(1),
                            Stroke::new(1.0_f32, t.text_muted),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }
        }
    }
}

fn prepare_clean_zdotdir() -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("cli-arai-zdotdir");
    std::fs::create_dir_all(&dir).ok()?;
    let zshrc = r#"# Load user's original zshrc for PATH, aliases, completions, etc.
if [ -f "$HOME/.zshrc" ]; then
  source "$HOME/.zshrc"
fi

# Remove theme precmd/preexec hooks (e.g. starship / p10k / execution timer) that inject RPROMPT or Nerd Font glyphs
precmd_functions=()
preexec_functions=()
unset RPROMPT RPS1

setopt PROMPT_SUBST
_cli_arai_git_branch() {
  local b
  b=$(git symbolic-ref --short HEAD 2>/dev/null || git rev-parse --short HEAD 2>/dev/null)
  if [ -n "$b" ]; then
    echo " %F{green}${b}%f"
  fi
}
PROMPT='%F{blue}%~%f$(_cli_arai_git_branch)
%F{white}>%f '
RPROMPT=''
"#;
    std::fs::write(dir.join(".zshrc"), zshrc).ok()?;
    Some(dir)
}

fn key_to_ctrl_byte(key: Key) -> Option<u8> {
    let b = match key {
        Key::A => 0x01,
        Key::B => 0x02,
        Key::C => 0x03,
        Key::D => 0x04,
        Key::E => 0x05,
        Key::F => 0x06,
        Key::G => 0x07,
        Key::H => 0x08,
        Key::I => 0x09,
        Key::J => 0x0a,
        Key::K => 0x0b,
        Key::L => 0x0c,
        Key::M => 0x0d,
        Key::N => 0x0e,
        Key::O => 0x0f,
        Key::P => 0x10,
        Key::Q => 0x11,
        Key::R => 0x12,
        Key::S => 0x13,
        Key::T => 0x14,
        Key::U => 0x15,
        Key::V => 0x16,
        Key::W => 0x17,
        Key::X => 0x18,
        Key::Y => 0x19,
        Key::Z => 0x1a,
        _ => return None,
    };
    Some(b)
}

fn vt_color_to_egui(c: vt100::Color, default: Color32, t: &ThemeColors) -> Color32 {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(idx) => ansi_256_to_color32(idx, t),
    }
}

fn ansi_256_to_color32(idx: u8, t: &ThemeColors) -> Color32 {
    match idx {
        // Standard 16 ANSI colors (tuned to match VS Code dark/light readability)
        0 => Color32::from_rgb(30, 34, 42),
        1 => t.accent_rose,
        2 => t.accent_emerald,
        3 => t.accent_amber,
        4 => t.accent_indigo,
        5 => t.accent_violet,
        6 => Color32::from_rgb(56, 189, 248),
        7 => Color32::from_rgb(212, 216, 224),
        8 => Color32::from_rgb(107, 114, 128),
        9 => Color32::from_rgb(251, 113, 133),
        10 => Color32::from_rgb(52, 211, 153),
        11 => Color32::from_rgb(251, 191, 36),
        12 => Color32::from_rgb(96, 165, 250),
        13 => Color32::from_rgb(192, 132, 252),
        14 => Color32::from_rgb(103, 232, 249),
        15 => Color32::from_rgb(248, 250, 252),
        // 6x6x6 color cube (16..=231)
        16..=231 => {
            let n = idx - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            let map = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(map(r), map(g), map(b))
        }
        // Grayscale ramp (232..=255)
        232..=255 => {
            let gray = 8 + (idx - 232) * 10;
            Color32::from_rgb(gray, gray, gray)
        }
    }
}
