//! The viewer's look: a dark and a light palette (following the system
//! setting unless chosen in the menu), the system's UI font when one is
//! found, and shared colors for custom-drawn parts.
//!
//! Everything here is set once at startup: no assets are bundled, and the
//! only file read is one UI font (about 1 MB), if present.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use eframe::egui::{
    self, epaint::Shadow, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle,
    Theme, Vec2, Visuals,
};

/// Colors for custom-drawn parts, per theme.
pub struct Palette {
    /// Behind the pages.
    pub canvas: Color32,
    /// Toolbar, sidebar and status bar.
    pub panel: Color32,
    /// Floating controls, menus and dialogs.
    pub raised: Color32,
    /// Hairlines between areas.
    pub border: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub accent: Color32,
    /// Text and icons on the accent color.
    pub on_accent: Color32,
    /// Accent, faintly, for selected controls.
    pub accent_soft: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub danger: Color32,
    pub warning: Color32,
    /// Shadows under pages and floating controls.
    pub shadow: Color32,
}

pub const DARK: Palette = Palette {
    canvas: Color32::from_rgb(28, 29, 33),
    panel: Color32::from_rgb(37, 38, 43),
    raised: Color32::from_rgb(47, 49, 55),
    border: Color32::from_rgb(52, 54, 61),
    text: Color32::from_rgb(228, 230, 235),
    weak: Color32::from_rgb(150, 154, 164),
    accent: Color32::from_rgb(98, 146, 255),
    on_accent: Color32::WHITE,
    accent_soft: Color32::from_rgba_premultiplied(24, 36, 64, 64),
    hover: Color32::from_rgba_premultiplied(18, 18, 20, 18),
    pressed: Color32::from_rgba_premultiplied(30, 30, 34, 30),
    danger: Color32::from_rgb(235, 87, 87),
    warning: Color32::from_rgb(240, 165, 50),
    shadow: Color32::from_black_alpha(110),
};

pub const LIGHT: Palette = Palette {
    canvas: Color32::from_rgb(226, 229, 234),
    panel: Color32::from_rgb(247, 248, 250),
    raised: Color32::from_rgb(255, 255, 255),
    border: Color32::from_rgb(218, 221, 227),
    text: Color32::from_rgb(30, 32, 37),
    weak: Color32::from_rgb(105, 110, 122),
    accent: Color32::from_rgb(45, 108, 240),
    on_accent: Color32::WHITE,
    accent_soft: Color32::from_rgba_premultiplied(9, 22, 48, 40),
    hover: Color32::from_rgba_premultiplied(0, 0, 0, 14),
    pressed: Color32::from_rgba_premultiplied(0, 0, 0, 26),
    danger: Color32::from_rgb(214, 54, 54),
    warning: Color32::from_rgb(196, 120, 0),
    shadow: Color32::from_black_alpha(50),
};

/// The palette in use for `ui`.
pub fn palette(ui: &egui::Ui) -> &'static Palette {
    of(ui.visuals())
}

pub fn of(visuals: &Visuals) -> &'static Palette {
    if visuals.dark_mode { &DARK } else { &LIGHT }
}

/// Sets up fonts, both themes and spacing.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(base_fonts());
    ctx.set_visuals_of(Theme::Dark, visuals(&DARK, Visuals::dark()));
    ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, Visuals::light()));
    ctx.all_styles_mut(|style| {
        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(6.0, 6.0);
        s.button_padding = Vec2::new(10.0, 5.0);
        s.interact_size.y = 28.0;
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(14);
        s.menu_spacing = 2.0;
        style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(20.0));
    });
}

fn visuals(p: &Palette, mut v: Visuals) -> Visuals {
    let radius = CornerRadius::same(7);
    v.panel_fill = p.panel;
    v.window_fill = p.raised;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: p.shadow };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: p.shadow };
    v.extreme_bg_color = p.canvas;
    v.text_edit_bg_color = Some(p.canvas);
    v.faint_bg_color = p.hover;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.override_text_color = None;

    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;
    // Buttons: a faint fill, no outline; stronger on hover.
    w.inactive.weak_bg_fill = p.hover;
    w.inactive.bg_fill = p.hover;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;
    w.hovered.weak_bg_fill = p.pressed;
    w.hovered.bg_fill = p.pressed;
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;
    w.active.weak_bg_fill = p.pressed;
    w.active.bg_fill = p.pressed;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;
    w.open.weak_bg_fill = p.pressed;
    w.open.bg_fill = p.pressed;
    w.open.bg_stroke = Stroke::NONE;
    w.open.corner_radius = radius;
    v
}

/// egui's fonts with the system UI font in front (for the editor, which
/// adds the document's fonts to these).
pub fn base_fonts() -> FontDefinitions {
    static FONT: OnceLock<Option<Arc<FontData>>> = OnceLock::new();
    let mut defs = FontDefinitions::default();
    if let Some(data) = FONT.get_or_init(|| system_ui_font().map(|d| Arc::new(FontData::from_owned(d)))) {
        defs.font_data.insert("ui".into(), data.clone());
        if let Some(list) = defs.families.get_mut(&FontFamily::Proportional) {
            list.insert(0, "ui".into());
        }
    }
    defs
}

/// The desktop's UI font, from the usual places (no font scan): Segoe UI
/// on Windows, San Francisco on macOS, and on Linux Adwaita Sans (GNOME),
/// Inter, Noto Sans or Cantarell. Only a font that parses is used (egui
/// would stop at one it can't read).
fn system_ui_font() -> Option<Vec<u8>> {
    #[cfg(windows)]
    let candidates: Vec<String> = {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        vec![format!("{windir}\\Fonts\\segoeui.ttf")]
    };
    #[cfg(target_os = "macos")]
    let candidates: Vec<String> =
        ["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc"].map(String::from).to_vec();
    #[cfg(not(any(windows, target_os = "macos")))]
    let candidates: Vec<String> = LINUX_FONTS.iter().map(|s| s.to_string()).collect();
    candidates
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .find(|data| ttf_parser::Face::parse(data, 0).is_ok())
}

#[cfg(not(any(windows, target_os = "macos")))]
const LINUX_FONTS: &[&str] = &[
    "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/truetype/adwaita-sans/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/adwaita-sans/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/rsms-inter-fonts/Inter-Regular.otf",
    "/usr/share/fonts/opentype/inter/Inter-Regular.otf",
    "/usr/share/fonts/inter/Inter-Regular.otf",
    "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/abattis-cantarell-fonts/Cantarell-Regular.otf",
    "/usr/share/fonts/cantarell/Cantarell-Regular.otf",
];

/// A button in the accent color, for a dialog's main action.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = palette(ui);
    ui.add(egui::Button::new(egui::RichText::new(text).color(p.on_accent)).fill(p.accent))
}

/// A button for an action that destroys something (e.g. redacting).
pub fn danger_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let p = palette(ui);
    ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(text).color(Color32::WHITE)).fill(p.danger))
}

/// A floating panel's frame (crop and redact panels, the page pill).
pub fn floating_frame(ui: &egui::Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(10, 6))
        .shadow(Shadow { offset: [0, 6], blur: 20, spread: 0, color: p.shadow })
}

/// Dark pages for reading: set by the viewer, read by everything drawn on
/// the page (form fields, the text editor) so it matches the rendering.
static DARK_PAGES: AtomicBool = AtomicBool::new(false);

pub fn set_dark_pages(dark: bool) {
    DARK_PAGES.store(dark, Ordering::Relaxed);
}

pub fn dark_pages() -> bool {
    DARK_PAGES.load(Ordering::Relaxed)
}

/// A color of the document as shown on the page: changed like the page
/// when pages are dark (alpha kept).
pub fn on_page(c: Color32) -> Color32 {
    if !dark_pages() {
        return c;
    }
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let [r, g, b] = k_pdf::dark_page_color([r, g, b]);
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// The page background: white, or the dark page color.
pub fn paper(dark: bool) -> Color32 {
    if dark {
        let [r, g, b] = k_pdf::dark_page_color([255, 255, 255]);
        Color32::from_rgb(r, g, b)
    } else {
        Color32::WHITE
    }
}

