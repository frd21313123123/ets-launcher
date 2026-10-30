//! Visual language of the launcher: palette, fonts, icons and painted controls.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Pos2, Rect, Response,
    RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use ets_launcher::settings::Theme;
use std::f32::consts::PI;

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub side: Color32,
    pub panel: Color32,
    pub inset: Color32,
    pub raised: Color32,
    pub hover: Color32,
    pub stripe1: Color32,
    pub stripe2: Color32,
    pub line_soft: Color32,
    pub line: Color32,
    pub line_strong: Color32,
    pub ring: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub accent_sel: Color32,
    pub accent_border: Color32,
    pub accent_soft: Color32,
    pub accent_chip: Color32,
    pub accent_glow: Color32,
    pub disabled: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub warn_bg: Color32,
    pub warn_border: Color32,
    pub warn_text: Color32,
    pub warn_chip: Color32,
    pub danger_border: Color32,
    pub danger_bg: Color32,
    pub danger_text: Color32,
}

const fn hex(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

pub const BLACK: Palette = Palette {
    dark: true,
    bg: hex(0x000000),
    side: hex(0x080808),
    panel: hex(0x111111),
    inset: hex(0x060606),
    raised: hex(0x1C1C1C),
    hover: hex(0x242424),
    stripe1: hex(0x141414),
    stripe2: hex(0x191919),
    line_soft: hex(0x1C1C1C),
    line: hex(0x2C2C2C),
    line_strong: hex(0x4A4A4A),
    ring: hex(0x2C2C2C),
    text: hex(0xF3EFEA),
    text2: hex(0xD8D2CB),
    text3: hex(0xBFB8B0),
    muted: hex(0x9B948C),
    accent: hex(0xF0A050),
    accent_hover: hex(0xF6BF85),
    on_accent: hex(0x1A0F05),
    accent_sel: hex(0x22170C),
    accent_border: hex(0x5E3F1E),
    accent_soft: hex(0x2B1E11),
    accent_chip: hex(0x22180E),
    accent_glow: hex(0x3B2713),
    disabled: hex(0x262320),
    ok: hex(0x74D69E),
    warn: hex(0xEBC65A),
    warn_bg: hex(0x1C180B),
    warn_border: hex(0x5A4C1E),
    warn_text: hex(0xF2E2B0),
    warn_chip: hex(0x29230F),
    danger_border: hex(0x5A2E2E),
    danger_bg: hex(0x170D0D),
    danger_text: hex(0xFF9E9E),
};

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: hex(0xF6F1EA),
    side: hex(0xEEE6DB),
    panel: hex(0xFFFFFF),
    inset: hex(0xFAF6F1),
    raised: hex(0xE7DCCF),
    hover: hex(0xE1D4C4),
    stripe1: hex(0xEFE6DB),
    stripe2: hex(0xE9DFD2),
    line_soft: hex(0xE6DBCE),
    line: hex(0xD8C9B8),
    line_strong: hex(0xB49F88),
    ring: hex(0xEADBC8),
    text: hex(0x2A1D15),
    text2: hex(0x46372C),
    text3: hex(0x5B4A3D),
    muted: hex(0x6F5F54),
    accent: hex(0xB4561A),
    accent_hover: hex(0x8C4113),
    on_accent: hex(0xFFFFFF),
    accent_sel: hex(0xF7E4D3),
    accent_border: hex(0xDDB08A),
    accent_soft: hex(0xF3DCC8),
    accent_chip: hex(0xF6E5D6),
    accent_glow: hex(0xF1D2B6),
    disabled: hex(0xE6DCD0),
    ok: hex(0x2E7D4F),
    warn: hex(0x875F00),
    warn_bg: hex(0xFBF3DC),
    warn_border: hex(0xE4CF8E),
    warn_text: hex(0x5A4300),
    warn_chip: hex(0xF6EBC8),
    danger_border: hex(0xE2B4AE),
    danger_bg: hex(0xFCEEEC),
    danger_text: hex(0xA3332A),
};

pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Black => BLACK,
        Theme::Light => LIGHT,
    }
}

/// Manrope weights used by the design (400/600/700/800).
#[derive(Clone, Copy)]
pub enum W {
    Regular,
    Semi,
    Bold,
    Extra,
}

impl W {
    fn family(self) -> &'static str {
        match self {
            W::Regular => "manrope-400",
            W::Semi => "manrope-600",
            W::Bold => "manrope-700",
            W::Extra => "manrope-800",
        }
    }
}

pub fn font(size: f32, weight: W) -> FontId {
    FontId::new(size, FontFamily::Name(weight.family().into()))
}

pub fn mono(size: f32, bold: bool) -> FontId {
    FontId::new(
        size,
        FontFamily::Name(if bold { "mono-700" } else { "mono-400" }.into()),
    )
}

pub fn text(value: impl Into<String>, size: f32, weight: W, color: Color32) -> RichText {
    RichText::new(value).font(font(size, weight)).color(color)
}

pub fn mono_text(value: impl Into<String>, size: f32, bold: bool, color: Color32) -> RichText {
    RichText::new(value).font(mono(size, bold)).color(color)
}

/// Section caption in capitals ("ИГРА", "РАЗДЕЛЫ").
pub fn overline(value: &str, p: &Palette) -> RichText {
    text(value, 11.0, W::Bold, p.muted)
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // Glyphs outside the embedded subsets (arrows, ≈) come from Windows fonts when present.
    let mut fallback = Vec::new();
    for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-symbol", "seguisym.ttf")] {
        if let Ok(bytes) = std::fs::read(format!("C:\\Windows\\Fonts\\{file}")) {
            fonts
                .font_data
                .insert(name.into(), egui::FontData::from_owned(bytes).into());
            fallback.push(name.to_owned());
        }
    }
    fallback.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    let mut add = |name: &str, latin: &'static [u8], cyrillic: &'static [u8]| {
        fonts.font_data.insert(
            format!("{name}-lat"),
            egui::FontData::from_static(latin).into(),
        );
        fonts.font_data.insert(
            format!("{name}-cyr"),
            egui::FontData::from_static(cyrillic).into(),
        );
        let mut chain = vec![format!("{name}-lat"), format!("{name}-cyr")];
        chain.extend(fallback.iter().cloned());
        fonts
            .families
            .insert(FontFamily::Name(name.into()), chain.clone());
        chain
    };
    let regular = add(
        "manrope-400",
        include_bytes!("../assets/fonts/manrope-lat-400.ttf"),
        include_bytes!("../assets/fonts/manrope-cyr-400.ttf"),
    );
    add(
        "manrope-600",
        include_bytes!("../assets/fonts/manrope-lat-600.ttf"),
        include_bytes!("../assets/fonts/manrope-cyr-600.ttf"),
    );
    add(
        "manrope-700",
        include_bytes!("../assets/fonts/manrope-lat-700.ttf"),
        include_bytes!("../assets/fonts/manrope-cyr-700.ttf"),
    );
    add(
        "manrope-800",
        include_bytes!("../assets/fonts/manrope-lat-800.ttf"),
        include_bytes!("../assets/fonts/manrope-cyr-800.ttf"),
    );
    let monospace = add(
        "mono-400",
        include_bytes!("../assets/fonts/jbmono-lat-400.ttf"),
        include_bytes!("../assets/fonts/jbmono-cyr-400.ttf"),
    );
    add(
        "mono-700",
        include_bytes!("../assets/fonts/jbmono-lat-700.ttf"),
        include_bytes!("../assets/fonts/jbmono-cyr-700.ttf"),
    );
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts.families.insert(FontFamily::Monospace, monospace);
    ctx.set_fonts(fonts);
}

pub fn apply_theme(ctx: &egui::Context, p: &Palette) {
    let mut visuals = if p.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = p.bg;
    visuals.window_fill = p.panel;
    visuals.window_stroke = Stroke::new(1.0_f32, p.line);
    visuals.extreme_bg_color = p.bg;
    visuals.faint_bg_color = p.inset;
    visuals.code_bg_color = p.inset;
    visuals.override_text_color = Some(p.text);
    visuals.hyperlink_color = p.accent;
    visuals.selection.bg_fill = p.accent_soft;
    visuals.selection.stroke = Stroke::new(1.0_f32, p.accent);
    visuals.text_cursor.stroke = Stroke::new(2.0_f32, p.accent);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(10);
    for (widget, fill, stroke) in [
        (&mut visuals.widgets.noninteractive, p.panel, p.line_soft),
        (&mut visuals.widgets.inactive, p.panel, p.line),
        (&mut visuals.widgets.hovered, p.hover, p.line_strong),
        (&mut visuals.widgets.active, p.raised, p.accent_border),
        (&mut visuals.widgets.open, p.raised, p.line),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::new(1.0_f32, stroke);
        widget.corner_radius = CornerRadius::same(10);
        widget.fg_stroke.color = p.text2;
    }
    visuals.widgets.noninteractive.fg_stroke.color = p.text2;
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = vec2(10.0, 10.0);
        style.spacing.button_padding = vec2(14.0, 9.0);
        style.spacing.interact_size.y = 36.0;
        style
            .text_styles
            .insert(egui::TextStyle::Body, font(14.0, W::Regular));
        style
            .text_styles
            .insert(egui::TextStyle::Button, font(14.0, W::Semi));
        style
            .text_styles
            .insert(egui::TextStyle::Small, font(12.0, W::Regular));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, font(20.0, W::Extra));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, mono(12.0, false));
    });
}

/// The design tints the mint logo with `hue-rotate(-125deg) saturate(1.15)`.
pub fn tint_logo(rgba: &mut [u8]) {
    let (s, c) = (-125f32).to_radians().sin_cos();
    let hue = [
        [
            0.213 + c * 0.787 - s * 0.213,
            0.715 - c * 0.715 - s * 0.715,
            0.072 - c * 0.072 + s * 0.928,
        ],
        [
            0.213 - c * 0.213 + s * 0.143,
            0.715 + c * 0.285 + s * 0.140,
            0.072 - c * 0.072 - s * 0.283,
        ],
        [
            0.213 - c * 0.213 - s * 0.787,
            0.715 - c * 0.715 + s * 0.715,
            0.072 + c * 0.928 + s * 0.072,
        ],
    ];
    let k = 1.15;
    let sat = [
        [0.213 + 0.787 * k, 0.715 - 0.715 * k, 0.072 - 0.072 * k],
        [0.213 - 0.213 * k, 0.715 + 0.285 * k, 0.072 - 0.072 * k],
        [0.213 - 0.213 * k, 0.715 - 0.715 * k, 0.072 + 0.928 * k],
    ];
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let input = [pixel[0] as f32, pixel[1] as f32, pixel[2] as f32];
        let rotated: [f32; 3] = std::array::from_fn(|i| (0..3).map(|j| hue[i][j] * input[j]).sum());
        for i in 0..3 {
            let value: f32 = (0..3).map(|j| sat[i][j] * rotated[j]).sum();
            pixel[i] = value.round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// A 34×34 tile of the diagonal placeholder stripes (135°, 12px bands).
pub fn stripe_tile(p: &Palette) -> egui::ColorImage {
    const SIZE: usize = 34;
    let period = SIZE as f32;
    let pixels = (0..SIZE * SIZE)
        .map(|i| {
            let (x, y) = ((i % SIZE) as f32 + 0.5, (i / SIZE) as f32 + 0.5);
            let phase = (x + y).rem_euclid(period) / period;
            if phase < 0.5 { p.stripe1 } else { p.stripe2 }
        })
        .collect();
    egui::ColorImage {
        size: [SIZE, SIZE],
        source_size: vec2(SIZE as f32, SIZE as f32),
        pixels,
    }
}

pub fn paint_stripes(
    ui: &Ui,
    tile: &egui::TextureHandle,
    rect: Rect,
    radius: impl Into<CornerRadius>,
) {
    let uv = Rect::from_min_size(Pos2::ZERO, rect.size() / 34.0);
    egui::Image::new(tile)
        .uv(uv)
        .corner_radius(radius)
        .paint_at(ui, rect);
}

/// Paints a cover image cropped to fill `rect` like `object-fit: cover`.
pub fn paint_cover(
    ui: &Ui,
    texture: &egui::TextureHandle,
    rect: Rect,
    radius: impl Into<CornerRadius>,
) {
    let size = texture.size_vec2();
    let (target, source) = (rect.width() / rect.height(), size.x / size.y.max(1.0));
    let uv = if source > target {
        let w = target / source;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = source / target;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    };
    egui::Image::new(texture)
        .uv(uv)
        .corner_radius(radius)
        .paint_at(ui, rect);
}

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Download,
    Layers,
    Sliders,
    Refresh,
    Check,
    Shield,
    Stop,
    External,
    Search,
    Folder,
    ChevronDown,
    ChevronLeft,
    Play,
    Warning,
}

/// Paints a 24×24 line icon (the same glyphs the mock-up uses) into `rect`.
pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32, width: f32) {
    let scale = rect.width() / 24.0;
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * scale;
    let stroke = Stroke::new(width * scale, color);
    let line = |points: &[(f32, f32)]| {
        painter.add(Shape::line(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    };
    let closed = |points: &[(f32, f32)]| {
        painter.add(Shape::closed_line(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    };
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| -> Vec<Pos2> {
        (0..=24)
            .map(|i| {
                let a = from + (to - from) * i as f32 / 24.0;
                at(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    };
    match icon {
        Icon::Download => {
            line(&[(12.0, 4.0), (12.0, 15.0)]);
            line(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]);
            line(&[(5.0, 20.0), (19.0, 20.0)]);
        }
        Icon::Layers => {
            closed(&[(12.0, 3.0), (3.0, 8.0), (12.0, 13.0), (21.0, 8.0)]);
            line(&[(3.0, 13.0), (12.0, 18.0), (21.0, 13.0)]);
        }
        Icon::Sliders => {
            line(&[(4.0, 7.0), (14.0, 7.0)]);
            line(&[(18.0, 7.0), (20.0, 7.0)]);
            line(&[(4.0, 17.0), (8.0, 17.0)]);
            line(&[(12.0, 17.0), (20.0, 17.0)]);
            painter.circle_stroke(at(16.0, 7.0), 2.0 * scale, stroke);
            painter.circle_stroke(at(10.0, 17.0), 2.0 * scale, stroke);
        }
        Icon::Refresh => {
            painter.add(Shape::line(arc(12.0, 11.0, 8.0, 0.0, -1.75 * PI), stroke));
            line(&[(20.0, 4.0), (20.0, 11.0), (13.0, 11.0)]);
        }
        Icon::Check => line(&[(5.0, 12.0), (10.0, 17.0), (19.0, 7.0)]),
        Icon::Shield => closed(&[
            (12.0, 3.0),
            (4.0, 6.0),
            (4.0, 12.0),
            (4.6, 15.0),
            (6.6, 18.0),
            (9.0, 19.9),
            (12.0, 21.0),
            (15.0, 19.9),
            (17.4, 18.0),
            (19.4, 15.0),
            (20.0, 12.0),
            (20.0, 6.0),
        ]),
        Icon::Stop => {
            painter.rect_stroke(
                Rect::from_min_max(at(6.0, 6.0), at(18.0, 18.0)),
                2.0 * scale,
                stroke,
                StrokeKind::Middle,
            );
        }
        Icon::External => {
            line(&[(14.0, 4.0), (20.0, 4.0), (20.0, 10.0)]);
            line(&[(20.0, 4.0), (11.0, 13.0)]);
            line(&[
                (18.0, 14.0),
                (18.0, 20.0),
                (4.0, 20.0),
                (4.0, 6.0),
                (10.0, 6.0),
            ]);
        }
        Icon::Search => {
            painter.circle_stroke(at(11.0, 11.0), 7.0 * scale, stroke);
            line(&[(20.0, 20.0), (16.5, 16.5)]);
        }
        Icon::Folder => closed(&[
            (3.0, 7.0),
            (5.0, 5.0),
            (9.0, 5.0),
            (11.0, 7.0),
            (19.0, 7.0),
            (21.0, 9.0),
            (21.0, 17.0),
            (19.0, 19.0),
            (5.0, 19.0),
            (3.0, 17.0),
        ]),
        Icon::ChevronDown => line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)]),
        Icon::ChevronLeft => line(&[(15.0, 6.0), (9.0, 12.0), (15.0, 18.0)]),
        Icon::Play => {
            painter.add(Shape::convex_polygon(
                vec![at(7.0, 5.0), at(18.0, 12.0), at(7.0, 19.0)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Warning => {
            closed(&[(12.0, 3.0), (2.0, 20.0), (22.0, 20.0)]);
            line(&[(12.0, 10.0), (12.0, 14.0)]);
            line(&[(12.0, 17.0), (12.0, 17.5)]);
        }
    }
}

/// Allocates a small square and paints an icon there.
pub fn icon(ui: &mut Ui, icon: Icon, size: f32, color: Color32, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_icon(ui.painter(), rect, icon, color, width);
    response
}

/// A small filled status dot.
pub fn dot(ui: &mut Ui, color: Color32, glow: Option<Color32>) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::splat(if glow.is_some() { 16.0 } else { 8.0 }),
        Sense::hover(),
    );
    if let Some(glow) = glow {
        ui.painter().circle_filled(rect.center(), 8.0, glow);
    }
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// Accent fill (main actions).
    Primary,
    /// Panel fill with a border.
    Ghost,
    /// Raised fill with a border ("Выбрать…").
    Raised,
    /// Red tinted ("Остановить").
    Danger,
    /// Transparent with a yellow border ("Показать").
    Warn,
}

pub struct Button<'a> {
    label: &'a str,
    icon: Option<Icon>,
    trailing: Option<Icon>,
    kind: Kind,
    height: f32,
    size: f32,
    weight: Option<W>,
    pad: f32,
    full_width: bool,
    enabled: bool,
    radius: u8,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str, kind: Kind) -> Self {
        Self {
            label,
            icon: None,
            trailing: None,
            kind,
            height: 44.0,
            size: 14.0,
            weight: None,
            pad: 16.0,
            full_width: false,
            enabled: true,
            radius: 10,
        }
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn trailing(mut self, icon: Icon) -> Self {
        self.trailing = Some(icon);
        self
    }
    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
    pub fn weight(mut self, weight: W) -> Self {
        self.weight = Some(weight);
        self
    }
    pub fn pad(mut self, pad: f32) -> Self {
        self.pad = pad;
        self
    }
    pub fn radius(mut self, radius: u8) -> Self {
        self.radius = radius;
        self
    }
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Width the button will take when shown (for right-aligned rows).
    pub fn width(&self, ui: &Ui) -> f32 {
        let galley = ui.painter().layout_no_wrap(
            self.label.to_owned(),
            font(self.size, self.text_weight()),
            Color32::PLACEHOLDER,
        );
        let icons = [self.icon, self.trailing].iter().flatten().count() as f32;
        galley.size().x + icons * ((self.size + 2.0).min(20.0) + 8.0) + self.pad * 2.0
    }

    fn text_weight(&self) -> W {
        self.weight.unwrap_or(match self.kind {
            Kind::Primary => W::Extra,
            Kind::Ghost => W::Semi,
            _ => W::Bold,
        })
    }

    pub fn show(self, ui: &mut Ui, p: &Palette) -> Response {
        let weight = self.weight.unwrap_or(match self.kind {
            Kind::Primary => W::Extra,
            Kind::Ghost => W::Semi,
            _ => W::Bold,
        });
        let galley = ui.painter().layout_no_wrap(
            self.label.to_owned(),
            font(self.size, weight),
            Color32::PLACEHOLDER,
        );
        let icon_size = (self.size + 2.0).min(20.0);
        let gap = 8.0;
        let extra = [self.icon, self.trailing].iter().flatten().count() as f32 * (icon_size + gap);
        let content = galley.size().x + extra;
        let width = if self.full_width {
            ui.available_width()
        } else {
            content + self.pad * 2.0
        };
        let (rect, response) = ui.allocate_exact_size(
            vec2(width, self.height),
            if self.enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if ui.is_rect_visible(rect) {
            let hovered = self.enabled && response.hovered();
            let (fill, border, color) = if !self.enabled {
                match self.kind {
                    Kind::Primary => (p.disabled, Color32::TRANSPARENT, p.muted),
                    _ => (p.side, p.line_soft, p.muted),
                }
            } else {
                match self.kind {
                    Kind::Primary => (
                        if hovered { p.accent_hover } else { p.accent },
                        Color32::TRANSPARENT,
                        p.on_accent,
                    ),
                    Kind::Ghost => (if hovered { p.hover } else { p.panel }, p.line, p.text2),
                    Kind::Raised => (if hovered { p.hover } else { p.raised }, p.line, p.text),
                    Kind::Danger => (
                        if hovered {
                            p.danger_border
                        } else {
                            p.danger_bg
                        },
                        p.danger_border,
                        p.danger_text,
                    ),
                    Kind::Warn => (
                        if hovered {
                            p.warn_chip
                        } else {
                            Color32::TRANSPARENT
                        },
                        p.warn_border,
                        p.warn,
                    ),
                }
            };
            let painter = ui.painter();
            painter.rect(
                rect,
                self.radius,
                fill,
                Stroke::new(1.0_f32, border),
                StrokeKind::Inside,
            );
            let mut x = if self.full_width {
                rect.center().x - content / 2.0
            } else {
                rect.min.x + self.pad
            };
            let stroke = if self.size >= 16.0 { 2.2 } else { 2.0 };
            if let Some(icon) = self.icon {
                paint_icon(
                    painter,
                    Rect::from_center_size(
                        pos2(x + icon_size / 2.0, rect.center().y),
                        Vec2::splat(icon_size),
                    ),
                    icon,
                    color,
                    stroke,
                );
                x += icon_size + gap;
            }
            let text_pos = pos2(x, rect.center().y - galley.size().y / 2.0);
            x += galley.size().x + gap;
            painter.galley(text_pos, galley, color);
            if let Some(icon) = self.trailing {
                paint_icon(
                    painter,
                    Rect::from_center_size(
                        pos2(x + icon_size / 2.0, rect.center().y),
                        Vec2::splat(icon_size - 2.0),
                    ),
                    icon,
                    color,
                    stroke,
                );
            }
        }
        if self.enabled {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        }
    }
}

/// A square icon-only button (44×44 refresh in the header).
pub fn icon_button(ui: &mut Ui, p: &Palette, icon: Icon, size: f32, enabled: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::splat(size),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let fill = if enabled && response.hovered() {
        p.hover
    } else {
        p.panel
    };
    ui.painter().rect(
        rect,
        10,
        fill,
        Stroke::new(1.0_f32, p.line),
        StrokeKind::Inside,
    );
    paint_icon(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(18.0)),
        icon,
        if enabled { p.text2 } else { p.muted },
        2.0,
    );
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// A text link painted in accent color with an optional trailing icon.
pub fn link(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    size: f32,
    weight: W,
    external: bool,
) -> Response {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), font(size, weight), Color32::PLACEHOLDER);
    let icon = if external { size + 1.0 } else { 0.0 };
    let (rect, response) = ui.allocate_exact_size(
        vec2(
            galley.size().x + if external { icon + 6.0 } else { 0.0 },
            galley.size().y.max(20.0),
        ),
        Sense::click(),
    );
    let color = if response.hovered() {
        p.accent_hover
    } else {
        p.accent
    };
    ui.painter().galley(
        pos2(rect.min.x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if external {
        paint_icon(
            ui.painter(),
            Rect::from_center_size(
                pos2(rect.max.x - icon / 2.0, rect.center().y),
                Vec2::splat(icon),
            ),
            Icon::External,
            color,
            2.0,
        );
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

pub struct Segment<'a> {
    pub label: &'a str,
    pub extra: Option<(String, bool, Color32)>,
    pub enabled: bool,
}

/// Segmented radio control; returns the clicked index.
#[allow(clippy::too_many_arguments)]
pub fn segmented(
    ui: &mut Ui,
    p: &Palette,
    selected: usize,
    segments: &[Segment],
    height: f32,
    size: f32,
    outer_fill: Color32,
    radius: u8,
) -> Option<usize> {
    let mut clicked = None;
    egui::Frame::new()
        .fill(outer_fill)
        .stroke(Stroke::new(1.0_f32, p.line))
        .corner_radius(radius)
        .inner_margin(4)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.horizontal(|ui| {
                // Inside a right-to-left parent the row is laid out backwards.
                let mut order = (0..segments.len()).collect::<Vec<_>>();
                if ui.layout().main_dir() == egui::Direction::RightToLeft {
                    order.reverse();
                }
                for index in order {
                    let segment = &segments[index];
                    let active = index == selected;
                    let weight = if active { W::Bold } else { W::Semi };
                    let color = if !segment.enabled {
                        p.muted
                    } else if active {
                        p.text
                    } else {
                        p.text3
                    };
                    let label = ui.painter().layout_no_wrap(
                        segment.label.to_owned(),
                        font(size, weight),
                        color,
                    );
                    let extra = segment.extra.as_ref().map(|(value, is_mono, color)| {
                        let font = if *is_mono {
                            mono(size - 2.0, false)
                        } else {
                            font(size - 2.0, W::Semi)
                        };
                        ui.painter().layout_no_wrap(value.clone(), font, *color)
                    });
                    let pad = if size >= 14.0 { 16.0 } else { 12.0 };
                    let width = label.size().x
                        + extra.as_ref().map_or(0.0, |g| g.size().x + 8.0)
                        + pad * 2.0;
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(width, height),
                        if segment.enabled {
                            Sense::click()
                        } else {
                            Sense::hover()
                        },
                    );
                    let fill = if active && segment.enabled {
                        p.accent_soft
                    } else if segment.enabled && response.hovered() {
                        p.hover
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter()
                        .rect_filled(rect, radius.saturating_sub(3), fill);
                    let mut x = rect.min.x + pad;
                    let label_width = label.size().x;
                    ui.painter().galley(
                        pos2(x, rect.center().y - label.size().y / 2.0),
                        label,
                        color,
                    );
                    x += label_width + 8.0;
                    if let Some(extra) = extra {
                        ui.painter().galley(
                            pos2(x, rect.center().y - extra.size().y / 2.0),
                            extra,
                            color,
                        );
                    }
                    if segment.enabled {
                        if response.clicked() {
                            clicked = Some(index);
                        }
                        response.on_hover_cursor(CursorIcon::PointingHand);
                    }
                }
            });
        });
    clicked
}

/// Rounded chip ("1.61 · v3", "Конвой").
pub fn chip(
    ui: &mut Ui,
    label: &str,
    is_mono: bool,
    color: Color32,
    fill: Color32,
    height: f32,
    size: f32,
) {
    let font = if is_mono {
        mono(size, true)
    } else {
        font(size, W::Semi)
    };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let pad = if height > 23.0 { 9.0 } else { 8.0 };
    let (rect, _) =
        ui.allocate_exact_size(vec2(galley.size().x + pad * 2.0, height), Sense::hover());
    ui.painter().rect_filled(rect, 6, fill);
    ui.painter().galley(
        pos2(rect.min.x + pad, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

/// Horizontal progress bar with a border.
pub fn progress(ui: &mut Ui, p: &Palette, fraction: f32, height: f32, fill: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let radius = (height / 2.0) as u8;
    ui.painter().rect(
        rect,
        radius,
        p.bg,
        Stroke::new(1.0_f32, p.line),
        StrokeKind::Inside,
    );
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let inner = rect.shrink(1.0);
        let filled = Rect::from_min_size(
            inner.min,
            vec2(
                (inner.width() * fraction).max(inner.height()),
                inner.height(),
            ),
        );
        ui.painter().rect_filled(filled, radius, fill);
    }
}

pub fn card(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.line))
        .corner_radius(16)
        .inner_margin(24)
}

pub fn inset(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.inset)
        .stroke(Stroke::new(1.0_f32, p.line_soft))
        .corner_radius(12)
}

/// Header of a page: muted subtitle above a large title.
pub fn page_title(ui: &mut Ui, p: &Palette, subtitle: &str, title: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(text(subtitle, 13.0, W::Semi, p.muted));
        ui.label(text(title, 32.0, W::Extra, p.text));
    });
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Warn,
    Info,
}

/// Inline banner for notes, warnings and errors. Returns true when the action was pressed.
pub fn banner(ui: &mut Ui, p: &Palette, tone: Tone, message: &str, action: Option<&str>) -> bool {
    let (fill, border, accent, body) = match tone {
        Tone::Warn => (p.warn_bg, p.warn_border, p.warn, p.warn_text),
        Tone::Info => (p.inset, p.line, p.accent, p.text2),
    };
    let mut pressed = false;
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                icon(
                    ui,
                    if tone == Tone::Info {
                        Icon::Shield
                    } else {
                        Icon::Warning
                    },
                    20.0,
                    accent,
                    2.0,
                );
                let reserve = if action.is_some() { 110.0 } else { 0.0 };
                ui.allocate_ui(vec2(ui.available_width() - reserve, 0.0), |ui| {
                    ui.add(egui::Label::new(text(message, 13.0, W::Regular, body)).wrap());
                });
                if let Some(action) = action {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        pressed = Button::new(action, Kind::Warn)
                            .height(40.0)
                            .size(13.0)
                            .pad(14.0)
                            .show(ui, p)
                            .clicked();
                    });
                }
            });
        });
    pressed
}

/// Single-line input in the design's 44px rounded field.
#[allow(clippy::too_many_arguments)]
pub fn field(
    ui: &mut Ui,
    p: &Palette,
    value: &mut String,
    hint: &str,
    is_mono: bool,
    width: f32,
    leading: Option<Icon>,
    fill: Color32,
) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 44.0), Sense::hover());
    let background = ui.painter().add(Shape::Noop);
    let mut inner = rect.shrink2(vec2(14.0, 0.0));
    if let Some(icon) = leading {
        paint_icon(
            ui.painter(),
            Rect::from_center_size(pos2(inner.min.x + 8.0, rect.center().y), Vec2::splat(16.0)),
            icon,
            p.muted,
            2.0,
        );
        inner.min.x += 26.0;
    }
    let font = if is_mono {
        mono(12.0, false)
    } else {
        font(14.0, W::Regular)
    };
    let response = ui.put(
        inner,
        egui::TextEdit::singleline(value)
            .frame(false)
            .font(font.clone())
            .text_color(p.text)
            .hint_text(RichText::new(hint).font(font).color(p.muted))
            .desired_width(inner.width())
            .vertical_align(egui::Align::Center),
    );
    let border = if response.has_focus() {
        p.accent_border
    } else {
        p.line
    };
    ui.painter().set(
        background,
        egui::epaint::RectShape::new(
            rect,
            10,
            fill,
            Stroke::new(1.0_f32, border),
            StrokeKind::Inside,
        ),
    );
    response
}

/// Text on the left, actions of a known width pinned to the right.
pub fn split_row(
    ui: &mut Ui,
    right_width: f32,
    left: impl FnOnce(&mut Ui),
    right: impl FnOnce(&mut Ui),
) {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
        let gap = ui.spacing().item_spacing.x;
        let left_width = (ui.available_width() - right_width - gap - 16.0).max(160.0);
        let inner = ui.allocate_ui_with_layout(
            vec2(left_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(left_width);
                left(ui);
            },
        );
        let height = inner.response.rect.height();
        ui.add_space(ui.available_width() - right_width - gap);
        ui.allocate_ui_with_layout(
            vec2(right_width, height),
            egui::Layout::left_to_right(egui::Align::Center),
            right,
        );
    });
}

/// "N мин назад" for a unix timestamp.
pub fn ago(timestamp: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.saturating_sub(timestamp);
    match age {
        0..60 => "только что".into(),
        60..3600 => format!("{} мин назад", age / 60),
        3600..86400 => format!("{} ч назад", age / 3600),
        _ => format!("{} дн назад", age / 86400),
    }
}

/// Centered text inside a painted rect.
pub fn paint_text(ui: &Ui, rect: Rect, value: &str, font: FontId, color: Color32) {
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, value, font, color);
}
