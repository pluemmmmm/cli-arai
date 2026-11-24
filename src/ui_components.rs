use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, Response, RichText, Stroke, Vec2};

#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    pub bg_canvas: Color32,
    pub bg_panel: Color32,
    pub bg_surface: Color32,
    pub bg_hover: Color32,
    pub bg_code: Color32,
    pub bg_gutter: Color32,

    pub border_hairline: Color32,
    pub border_subtle: Color32,

    pub accent_indigo: Color32,
    pub accent_violet: Color32,
    pub accent_cyan: Color32,
    pub accent_emerald: Color32,
    pub accent_amber: Color32,
    pub accent_rose: Color32,

    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,

    pub diff_add_bg: Color32,
    pub diff_add_gutter: Color32,
    pub diff_add_fg: Color32,

    pub diff_del_bg: Color32,
    pub diff_del_gutter: Color32,
    pub diff_del_fg: Color32,

    pub diff_hdr_bg: Color32,
}

impl ThemeColors {
    /// Standard Neutral Developer / CLI Dark Theme (VS Code Dark Modern / GitHub Dark style)
    pub const fn dark() -> Self {
        Self {
            bg_canvas: Color32::from_rgb(24, 24, 24),   // #181818
            bg_panel: Color32::from_rgb(31, 31, 31),    // #1F1F1F
            bg_surface: Color32::from_rgb(42, 42, 42),  // #2A2A2A
            bg_hover: Color32::from_rgb(51, 51, 51),    // #333333
            bg_code: Color32::from_rgb(20, 20, 20),     // #141414
            bg_gutter: Color32::from_rgb(26, 26, 26),   // #1A1A1A

            border_hairline: Color32::from_rgb(46, 46, 46), // #2E2E2E
            border_subtle: Color32::from_rgb(64, 64, 64),   // #404040

            accent_indigo: Color32::from_rgb(59, 130, 246),  // #3B82F6 (Standard CLI Blue)
            accent_violet: Color32::from_rgb(192, 132, 252), // #C084FC (ANSI Magenta)
            accent_cyan: Color32::from_rgb(56, 189, 248),    // #38BDF8 (ANSI Cyan)
            accent_emerald: Color32::from_rgb(74, 222, 128), // #4ADE80 (ANSI Green)
            accent_amber: Color32::from_rgb(250, 204, 21),   // #FACC15 (ANSI Yellow)
            accent_rose: Color32::from_rgb(248, 113, 113),   // #F87171 (ANSI Red)

            text_primary: Color32::from_rgb(228, 228, 231),   // #E4E4E7
            text_secondary: Color32::from_rgb(161, 161, 170), // #A1A1AA
            text_muted: Color32::from_rgb(113, 113, 122),     // #71717A

            diff_add_bg: Color32::from_rgb(23, 43, 31),
            diff_add_gutter: Color32::from_rgb(29, 56, 39),
            diff_add_fg: Color32::from_rgb(134, 239, 172),

            diff_del_bg: Color32::from_rgb(47, 25, 28),
            diff_del_gutter: Color32::from_rgb(61, 32, 36),
            diff_del_fg: Color32::from_rgb(252, 165, 165),

            diff_hdr_bg: Color32::from_rgb(29, 38, 54),
        }
    }

    /// Standard Neutral Developer / CLI Light Theme (VS Code Light Modern / GitHub Light style)
    pub const fn light() -> Self {
        Self {
            bg_canvas: Color32::from_rgb(243, 243, 243), // #F3F3F3
            bg_panel: Color32::from_rgb(255, 255, 255),  // #FFFFFF
            bg_surface: Color32::from_rgb(236, 236, 236), // #ECECEC
            bg_hover: Color32::from_rgb(224, 224, 224),  // #E0E0E0
            bg_code: Color32::from_rgb(255, 255, 255),   // #FFFFFF
            bg_gutter: Color32::from_rgb(247, 247, 247), // #F7F7F7

            border_hairline: Color32::from_rgb(229, 229, 229), // #E5E5E5
            border_subtle: Color32::from_rgb(212, 212, 212),   // #D4D4D4

            accent_indigo: Color32::from_rgb(0, 102, 204),  // #0066CC (Standard CLI Blue)
            accent_violet: Color32::from_rgb(124, 58, 237), // #7C3AED
            accent_cyan: Color32::from_rgb(3, 105, 161),    // #0369A1
            accent_emerald: Color32::from_rgb(21, 128, 61), // #15803D
            accent_amber: Color32::from_rgb(180, 83, 9),    // #B45309
            accent_rose: Color32::from_rgb(220, 38, 38),    // #DC2626

            text_primary: Color32::from_rgb(24, 24, 27),    // #18181B
            text_secondary: Color32::from_rgb(82, 82, 91),  // #52525B
            text_muted: Color32::from_rgb(113, 113, 122),   // #71717A

            diff_add_bg: Color32::from_rgb(230, 255, 236),
            diff_add_gutter: Color32::from_rgb(204, 255, 216),
            diff_add_fg: Color32::from_rgb(17, 99, 41),

            diff_del_bg: Color32::from_rgb(255, 235, 233),
            diff_del_gutter: Color32::from_rgb(255, 215, 213),
            diff_del_fg: Color32::from_rgb(130, 7, 30),

            diff_hdr_bg: Color32::from_rgb(221, 244, 255),
        }
    }
}

/// Return the currently active `ThemeColors` based on `egui`'s dark/light mode.
pub fn active_theme(ctx: &egui::Context) -> ThemeColors {
    if ctx.style().visuals.dark_mode {
        ThemeColors::dark()
    } else {
        ThemeColors::light()
    }
}

/// Apply either the refined Dark or Light theme to `egui::Context`.
pub fn apply_theme(ctx: &egui::Context, dark: bool) {
    let t = if dark {
        ThemeColors::dark()
    } else {
        ThemeColors::light()
    };

    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    visuals.panel_fill = t.bg_canvas;
    visuals.window_fill = t.bg_panel;
    visuals.extreme_bg_color = t.bg_code;
    visuals.faint_bg_color = t.bg_surface;

    visuals.widgets.noninteractive.bg_fill = t.bg_panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, t.border_hairline);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.text_primary);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(7);

    visuals.widgets.inactive.bg_fill = t.bg_surface;
    visuals.widgets.inactive.weak_bg_fill = t.bg_surface;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, t.border_hairline);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, t.text_secondary);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(7);

    visuals.widgets.hovered.bg_fill = t.bg_hover;
    visuals.widgets.hovered.weak_bg_fill = t.bg_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, t.border_subtle);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, t.text_primary);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(7);

    visuals.widgets.active.bg_fill = t.accent_indigo;
    visuals.widgets.active.weak_bg_fill = t.accent_indigo;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, t.accent_indigo);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.active.corner_radius = CornerRadius::same(7);

    visuals.widgets.open.bg_fill = t.bg_surface;
    visuals.widgets.open.weak_bg_fill = t.bg_surface;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, t.border_hairline);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, t.text_primary);
    visuals.widgets.open.corner_radius = CornerRadius::same(7);

    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(
        t.accent_indigo.r(),
        t.accent_indigo.g(),
        t.accent_indigo.b(),
        if dark { 65 } else { 45 },
    );
    visuals.selection.stroke = Stroke::new(1.0, t.accent_indigo);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.window_stroke = Stroke::new(1.0, t.border_hairline);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    ctx.set_style(style);
}

/// Consistent painted circular indicator dot (never renders as a missing font glyph).
pub fn status_dot(ui: &mut egui::Ui, color: Color32, radius: f32) {
    let size = (radius * 2.0 + 2.0).max(8.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Crisp vector Git Branch icon (Lucide-style, never renders as a missing font glyph).
pub fn icon_git_branch(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let p = ui.painter();
    let s = Stroke::new(1.5, color);
    let x1 = rect.left() + size * 0.28;
    let x2 = rect.left() + size * 0.72;
    let y_top = rect.top() + size * 0.24;
    let y_bot = rect.bottom() - size * 0.24;
    let r = size * 0.13;

    // Vertical main stem
    p.line_segment([egui::pos2(x1, y_top + r), egui::pos2(x1, y_bot - r)], s);
    // Branch fork arm from bottom-left to top-right
    p.line_segment(
        [egui::pos2(x1, y_bot - r * 1.4), egui::pos2(x2, y_top + r * 1.4)],
        s,
    );
    // Three branch nodes
    p.circle_stroke(egui::pos2(x1, y_top), r, s);
    p.circle_stroke(egui::pos2(x1, y_bot), r, s);
    p.circle_stroke(egui::pos2(x2, y_top), r, s);
}

/// Crisp vector Sun icon for Light mode toggle.
pub fn icon_sun(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    let s = Stroke::new(1.4, color);
    let r_inner = size * 0.20;
    let r_ray0 = size * 0.32;
    let r_ray1 = size * 0.44;
    p.circle_stroke(c, r_inner, s);
    for i in 0..8 {
        let angle = (i as f32) * std::f32::consts::FRAC_PI_4;
        let (sin, cos) = angle.sin_cos();
        p.line_segment(
            [
                egui::pos2(c.x + cos * r_ray0, c.y + sin * r_ray0),
                egui::pos2(c.x + cos * r_ray1, c.y + sin * r_ray1),
            ],
            s,
        );
    }
}

/// Crisp vector Moon icon for Dark mode toggle.
pub fn icon_moon(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    let r = size * 0.34;
    p.circle_filled(c, r, color);
    let bg = ui.visuals().panel_fill;
    p.circle_filled(egui::pos2(c.x + r * 0.45, c.y - r * 0.25), r * 0.82, bg);
}

/// Crisp vector Bottom-Panel layout icon.
pub fn icon_panel(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let p = ui.painter();
    let box_rect = rect.shrink(size * 0.14);
    let s = Stroke::new(1.3, color);
    p.rect_stroke(box_rect, CornerRadius::same(2), s, egui::StrokeKind::Inside);
    let split_y = box_rect.top() + box_rect.height() * 0.62;
    p.line_segment(
        [
            egui::pos2(box_rect.left(), split_y),
            egui::pos2(box_rect.right(), split_y),
        ],
        s,
    );
}

/// Crisp vector Refresh icon (circular arc + small arrowhead).
pub fn icon_refresh(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    let r = size * 0.33;
    let s = Stroke::new(1.4, color);
    let steps = 14;
    let start_a = -0.4f32;
    let end_a = std::f32::consts::TAU - 1.1;
    let mut prev = egui::pos2(c.x + start_a.cos() * r, c.y + start_a.sin() * r);
    for i in 1..=steps {
        let a = start_a + (end_a - start_a) * (i as f32 / steps as f32);
        let pt = egui::pos2(c.x + a.cos() * r, c.y + a.sin() * r);
        p.line_segment([prev, pt], s);
        prev = pt;
    }
    // Arrowhead at end
    let tip = prev;
    p.line_segment([tip, egui::pos2(tip.x - 3.0, tip.y - 2.0)], s);
    p.line_segment([tip, egui::pos2(tip.x + 1.5, tip.y - 3.2)], s);
}

/// Crisp vector Settings gear icon.
pub fn icon_settings(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    let s = Stroke::new(1.35, color);
    let r_inner = size * 0.16;
    let r_outer = size * 0.30;
    let r_tooth = size * 0.43;
    p.circle_stroke(c, r_inner, s);
    p.circle_stroke(c, r_outer, s);
    for i in 0..6 {
        let angle = (i as f32) * (std::f32::consts::TAU / 6.0);
        let (sin, cos) = angle.sin_cos();
        p.line_segment(
            [
                egui::pos2(c.x + cos * r_outer, c.y + sin * r_outer),
                egui::pos2(c.x + cos * r_tooth, c.y + sin * r_tooth),
            ],
            s,
        );
    }
}

/// Crisp vector Image attachment icon (frame + mountain + sun).
pub fn icon_image(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let p = ui.painter();
    let box_rect = rect.shrink(size * 0.12);
    let s = Stroke::new(1.3, color);
    p.rect_stroke(box_rect, CornerRadius::same(2), s, egui::StrokeKind::Inside);
    // Small sun dot
    p.circle_filled(
        egui::pos2(
            box_rect.left() + box_rect.width() * 0.30,
            box_rect.top() + box_rect.height() * 0.32,
        ),
        size * 0.08,
        color,
    );
    // Mountain peaks
    let p1 = egui::pos2(box_rect.left() + 1.5, box_rect.bottom() - 1.5);
    let p2 = egui::pos2(
        box_rect.left() + box_rect.width() * 0.45,
        box_rect.top() + box_rect.height() * 0.55,
    );
    let p3 = egui::pos2(
        box_rect.left() + box_rect.width() * 0.65,
        box_rect.bottom() - 2.5,
    );
    let p4 = egui::pos2(
        box_rect.right() - 1.5,
        box_rect.top() + box_rect.height() * 0.45,
    );
    p.line_segment([p1, p2], s);
    p.line_segment([p2, p3], s);
    p.line_segment([p3, p4], s);
}

/// Ghost button with a crisp vector icon on the left + label on the right.
pub fn ghost_icon_button(
    ui: &mut egui::Ui,
    label: &str,
    draw_icon: impl FnOnce(&mut egui::Ui, Color32, f32),
) -> Response {
    let t = active_theme(ui.ctx());
    let id = ui.make_persistent_id(("ghost_icon_btn", label));
    let hovered = ui.ctx().memory(|m| m.data.get_temp::<bool>(id).unwrap_or(false));
    let bg = if hovered { t.bg_hover } else { Color32::TRANSPARENT };
    let stroke = if hovered {
        Stroke::new(1.0, t.border_subtle)
    } else {
        Stroke::NONE
    };
    let fg = if hovered { t.text_primary } else { t.text_secondary };

    let is_rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
    let resp = Frame::new()
        .fill(bg)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if is_rtl {
                    ui.label(RichText::new(label).size(11.5).color(fg));
                    draw_icon(ui, fg, 14.0);
                } else {
                    draw_icon(ui, fg, 14.0);
                    ui.label(RichText::new(label).size(11.5).color(fg));
                }
            });
        })
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    ui.ctx()
        .memory_mut(|m| m.data.insert_temp(id, resp.hovered()));
    resp
}

/// Crisp, clearly visible Modal Close (`X`) icon button with subtle surface + red-tinted hover.
pub fn modal_close_button(ui: &mut egui::Ui) -> Response {
    let t = active_theme(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(26.0), egui::Sense::click());
    let resp = resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Close (Esc)");

    let hovered = resp.hovered();
    let bg = if hovered {
        t.diff_del_bg
    } else {
        t.bg_surface
    };
    let border = if hovered {
        t.accent_rose
    } else {
        t.border_subtle
    };
    let fg = if hovered {
        t.accent_rose
    } else {
        t.text_primary
    };

    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(6),
        bg,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    let c = rect.center();
    let d = 4.5;
    let s = Stroke::new(1.7, fg);
    p.line_segment([egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)], s);
    p.line_segment([egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)], s);

    resp
}

/// Unified section header used across CHANGED FILES, AI CHAT, and TERMINAL.
pub fn section_title(ui: &mut egui::Ui, title: &str, dot_color: Color32) {
    let t = active_theme(ui.ctx());
    status_dot(ui, dot_color, 3.8);
    ui.label(
        RichText::new(title)
            .size(11.5)
            .strong()
            .color(t.text_secondary),
    );
}

/// Consistent pill badge for statuses, counts, and diff stats.
pub fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).strong().color(fg));
        });
}

/// Consistent status colors for git file statuses (`NEW`, `A`, `M`, `D`).
pub fn status_colors(t: &ThemeColors, status: &str) -> (Color32, Color32) {
    match status {
        "A" | "NEW" | "??" => (t.accent_emerald, t.diff_add_bg),
        "D" => (t.accent_rose, t.diff_del_bg),
        _ => (
            t.accent_amber,
            Color32::from_rgba_unmultiplied(t.accent_amber.r(), t.accent_amber.g(), t.accent_amber.b(), 28),
        ),
    }
}

/// Ghost button: transparent at rest, subtle hover fill + border + bright text + pointer cursor.
pub fn ghost_button(ui: &mut egui::Ui, label: &str) -> Response {
    let t = active_theme(ui.ctx());
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_stroke = Stroke::NONE;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, t.text_secondary);
        v.widgets.hovered.weak_bg_fill = t.bg_hover;
        v.widgets.hovered.bg_fill = t.bg_hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, t.border_subtle);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, t.text_primary);

        ui.spacing_mut().button_padding = Vec2::new(8.0, 4.0);
        ui.add(
            egui::Button::new(RichText::new(label).size(11.5))
                .corner_radius(CornerRadius::same(6)),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
    })
    .inner
}

/// Toggle switch control for top-bar layout toggles (e.g. Diff / AI / Terminal).
/// Drawn with egui painter (no Unicode glyphs) and smooth macOS-style sliding thumb animation.
pub fn panel_toggle_button(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    accent: Color32,
) -> Response {
    let t = active_theme(ui.ctx());
    let font_id = egui::FontId::proportional(11.5);
    let text_color = if active {
        t.text_primary
    } else {
        t.text_muted
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, text_color);

    let pad_x = 8.0_f32;
    let gap = 6.0_f32;
    let track_w = 24.0_f32;
    let track_h = 13.0_f32;
    let total_w = pad_x + galley.size().x + gap + track_w + pad_x;
    let total_h = 24.0_f32;

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, total_h), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

    let anim = ui.ctx().animate_bool_responsive(resp.id, active);

    if ui.is_rect_visible(rect) {
        let p = ui.painter();

        // Subtle container background on hover
        let container_bg = if resp.hovered() {
            t.bg_hover
        } else if active {
            t.bg_surface
        } else {
            Color32::TRANSPARENT
        };
        let container_stroke = if resp.hovered() {
            Stroke::new(1.0_f32, t.border_subtle)
        } else if active {
            Stroke::new(1.0_f32, t.border_hairline)
        } else {
            Stroke::NONE
        };
        p.rect(
            rect,
            CornerRadius::same(7),
            container_bg,
            container_stroke,
            egui::StrokeKind::Inside,
        );

        // Label on the left
        let text_pos = egui::pos2(
            rect.min.x + pad_x,
            rect.center().y - galley.size().y * 0.5,
        );
        p.galley(text_pos, galley, text_color);

        // Toggle switch track on the right
        let track_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.max.x - pad_x - track_w,
                rect.center().y - track_h * 0.5,
            ),
            Vec2::new(track_w, track_h),
        );
        let track_radius = CornerRadius::same((track_h * 0.5) as u8);
        let off_track = t.bg_hover;
        let track_color = Color32::from_rgb(
            egui::lerp(off_track.r() as f32..=accent.r() as f32, anim) as u8,
            egui::lerp(off_track.g() as f32..=accent.g() as f32, anim) as u8,
            egui::lerp(off_track.b() as f32..=accent.b() as f32, anim) as u8,
        );
        p.rect_filled(track_rect, track_radius, track_color);
        if !active {
            p.rect_stroke(
                track_rect,
                track_radius,
                Stroke::new(1.0_f32, t.border_subtle),
                egui::StrokeKind::Inside,
            );
        }

        // Sliding circular thumb
        let thumb_r = (track_h * 0.5) - 2.0;
        let min_x = track_rect.min.x + thumb_r + 2.0;
        let max_x = track_rect.max.x - thumb_r - 2.0;
        let thumb_x = egui::lerp(min_x..=max_x, anim);
        let thumb_color = if active {
            Color32::WHITE
        } else {
            t.text_secondary
        };
        p.circle_filled(
            egui::pos2(thumb_x, track_rect.center().y),
            thumb_r,
            thumb_color,
        );
    }

    resp
}

/// Subtle outlined button: uniform `bg_surface` fill + `border_hairline` + smooth hover + pointer cursor.
pub fn subtle_button(ui: &mut egui::Ui, label: &str) -> Response {
    let t = active_theme(ui.ctx());
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.widgets.inactive.weak_bg_fill = t.bg_surface;
        v.widgets.inactive.bg_fill = t.bg_surface;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, t.border_hairline);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, t.text_secondary);
        v.widgets.hovered.weak_bg_fill = t.bg_hover;
        v.widgets.hovered.bg_fill = t.bg_hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, t.border_subtle);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, t.text_primary);

        ui.spacing_mut().button_padding = Vec2::new(9.0, 4.0);
        ui.add(
            egui::Button::new(RichText::new(label).size(11.5))
                .corner_radius(CornerRadius::same(6)),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
    })
    .inner
}

/// Primary accent button: filled with active theme accent when enabled, muted surface when disabled.
pub fn primary_button(ui: &mut egui::Ui, label: &str, accent: Color32, enabled: bool) -> Response {
    let t = active_theme(ui.ctx());
    let fill = if enabled { accent } else { t.bg_hover };
    let fg = if enabled { Color32::WHITE } else { t.text_muted };
    let resp = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).size(12.0).strong().color(fg))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(62.0, 26.0)),
    );
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Reusable segmented toggle control (e.g. `Vertical (Y)` / `Split (X)`).
pub fn segmented_toggle<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    current: &mut T,
    options: &[(T, &str)],
    accent: Color32,
) {
    let t = active_theme(ui.ctx());
    let is_rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
    Frame::new()
        .fill(t.bg_canvas)
        .stroke(Stroke::new(1.0, t.border_hairline))
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let iter: Vec<&(T, &str)> = if is_rtl {
                    options.iter().rev().collect()
                } else {
                    options.iter().collect()
                };
                for (val, label) in iter {
                    let active = *current == *val;
                    let resp = ui
                        .add(
                            egui::Button::new(
                                RichText::new(*label).size(11.5).color(if active {
                                    Color32::WHITE
                                } else {
                                    t.text_muted
                                }),
                            )
                            .fill(if active { accent } else { Color32::TRANSPARENT })
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(5)),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.clicked() {
                        *current = *val;
                    }
                }
            });
        });
}
