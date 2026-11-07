//! k-pdf: a small desktop PDF viewer and editor.
//!
//! Usage: `k-pdf [file.pdf] [--page N] [--screenshot out.ppm]`, or
//! open a file from the toolbar (Ctrl+O) or by dropping it onto the window.
//!
//! `--screenshot` saves the window once the visible pages have rendered and
//! then exits; it exists for automated checks of the UI.

// A window application: no console window on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod compress;
mod crop;
mod editor;
mod comments;
mod form;
mod icons;
mod links;
mod present;
mod print;
mod redact;
mod rotate;
#[cfg(feature = "script")]
mod script;
mod selection;
mod sidebar;
mod theme;
mod titlebar;
mod worker;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CursorIcon, Key, Modifiers, Pos2, Rect, Sense, Vec2};
use k_pdf::{Link, LinkTarget, TextChar};
use icons::Icon;
use selection::{Move, Selection};
use worker::{DocInfo, Pool, Reply};

/// Space around and between pages (points).
const MARGIN: f32 = 16.0;
const GAP: f32 = 14.0;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
const ZOOM_STEP: f32 = 1.2;
/// Mouse wheel distance per notch (egui's default is 40 points).
const WHEEL_SCROLL: f32 = 110.0;
/// Rendered pages are kept until they use more texture memory than this.
const TEXTURE_BUDGET: usize = 256 * 1024 * 1024;
/// After zooming, wait this long before re-rendering at the new size.
const ZOOM_SETTLE: Duration = Duration::from_millis(150);
/// rgb(50, 120, 255) at about 31% opacity, premultiplied.
const SELECTION_COLOR: Color32 = Color32::from_rgba_premultiplied(16, 38, 80, 80);

/// Command-line options.
#[derive(Default)]
struct Args {
    path: Option<PathBuf>,
    page: Option<usize>,
    screenshot: Option<PathBuf>,
    #[cfg(feature = "script")]
    script: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args_os().skip(1);
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--page") => args.page = it.next().and_then(|v| v.to_str()?.parse::<usize>().ok()),
            Some("--screenshot") => args.screenshot = it.next().map(PathBuf::from),
            #[cfg(feature = "script")]
            Some("--script") => args.script = it.next().map(PathBuf::from),
            Some("--version" | "-V") => {
                println!("k-pdf {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            Some("--help" | "-h") => {
                println!("k-pdf {}: a PDF viewer and editor\n\nUsage: k-pdf [file.pdf] [--page N]", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => args.path = Some(PathBuf::from(a)),
        }
    }
    args
}

fn main() -> eframe::Result {
    let args = parse_args();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("k-pdf")
            // Matches k-pdf.desktop, so desktops show the right name and icon.
            .with_app_id("k-pdf")
            .with_icon(egui::IconData {
                rgba: include_bytes!("../assets/icon-128.rgba").to_vec(),
                width: 128,
                height: 128,
            })
            .with_inner_size([1000.0, 800.0])
            .with_min_inner_size([420.0, 300.0])
            .with_decorations(titlebar::NATIVE) // we draw our own title bar (not on macOS)
            .with_drag_and_drop(true),
        // Without vsync, a window the compositor doesn't draw (hidden, or
        // on a screen that isn't shown) can't hold up the app: with it,
        // finishing a frame waits for the compositor, which on Wayland
        // never comes for such a window, and the app freezes. The viewer
        // only repaints when something changes, and animations ask for
        // about 60 frames a second (see FRAME).
        glow_options: eframe::egui_glow::GlowConfiguration { vsync: false, ..Default::default() },
        ..Default::default()
    };
    eframe::run_native("k-pdf", options, Box::new(|cc| Ok(Box::new(Viewer::new(cc, args)))))
}

/// Time between frames while something moves (about 60 a second).
pub const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, PartialEq)]
enum Fit {
    None,
    Width,
    Page,
}

/// A bitmap rendered within this relative distance of the wanted scale is
/// kept and drawn pixel for pixel (less than a pixel off across a page).
const SCALE_TOLERANCE: f32 = 0.001;

struct PageTexture {
    texture: egui::TextureHandle,
    scale: f32,
    bytes: usize,
    last_used: u64,
}

/// Label/value rows; long values (paths, titles, keywords) wrap within the
/// window's width.
fn info_rows(ui: &mut egui::Ui, rows: &[(String, String)]) {
    const LABEL_WIDTH: f32 = 100.0;
    for (label, value) in rows {
        ui.horizontal_top(|ui| {
            ui.add_sized([LABEL_WIDTH, 0.0], egui::Label::new(egui::RichText::new(label).weak()).halign(egui::Align::LEFT));
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(value).wrap());
            });
        });
        ui.add_space(4.0);
    }
}

/// Where to draw a page bitmap so it stays sharp. When the bitmap was
/// rendered for (about) the current zoom, it is drawn at its exact pixel
/// size on a whole-pixel position, so every bitmap pixel lands on one screen
/// pixel. Otherwise (while a zoom settles) it is stretched over the page.
fn crisp_rect(page: Rect, t: &PageTexture, target_scale: f32, ppp: f32) -> Rect {
    if (t.scale / target_scale - 1.0).abs() > SCALE_TOLERANCE {
        return page;
    }
    let snap = |v: f32| (v * ppp).round() / ppp;
    let [w, h] = t.texture.size();
    Rect::from_min_size(
        Pos2::new(snap(page.min.x), snap(page.min.y)),
        Vec2::new(w as f32 / ppp, h as f32 / ppp),
    )
}

/// One step Ctrl+Z can take back.
enum Undo {
    /// The page order before a move or an insertion.
    Order(Vec<usize>),
    /// The text edits and crops before an edit or crop.
    Text(k_pdf::Changes),
}

/// Where comment controls attach on screen this frame.
#[derive(Default)]
struct CommentAnchors {
    /// The comment whose box is open.
    open: Option<Rect>,
    /// The first line of the selection (for the highlight bar).
    selection: Option<Rect>,
}

/// What to do once unsaved changes have been saved or discarded.
enum After {
    Nothing,
    Close,
    Open(PathBuf),
}

struct Saving {
    target: PathBuf,
    result: std::sync::mpsc::Receiver<Result<(), String>>,
    after: After,
}

/// Writes a PDF made of `pages` ((file, page) pairs; file 0 is the base
/// document, whose metadata, outline and form are kept), with the text
/// edits in `text_changes` and the form fields in `form_edits`, to `target`. The new file
/// is written next to the target and then renamed over it, so the target is
/// never left half-written (and the open, memory-mapped originals aren't
/// modified underneath the viewer).
fn save_pages(
    files: &[PathBuf],
    pages: &[(usize, usize)],
    form_edits: &[(k_pdf::ObjRef, k_pdf::FieldValue)],
    text_changes: &k_pdf::Changes,
    target: &std::path::Path,
) -> Result<(), String> {
    write_atomically(target, |file| build_pdf(files, pages, form_edits, text_changes, file))
}

/// Writes the PDF described as for [`save_pages`] to `out`.
fn build_pdf(
    files: &[PathBuf],
    pages: &[(usize, usize)],
    form_edits: &[(k_pdf::ObjRef, k_pdf::FieldValue)],
    text_changes: &k_pdf::Changes,
    out: impl std::io::Write,
) -> Result<(), String> {
    // Open the files that are used (inserted files whose pages were all
    // undone may be gone or unreadable by now).
    let mut docs: Vec<Option<k_pdf::Document>> = Vec::new();
    for (i, path) in files.iter().enumerate() {
        let used = i == 0 || pages.iter().any(|&(d, _)| d == i);
        docs.push(if used { Some(k_pdf::Document::open(path).map_err(|e| e.to_string())?) } else { None });
    }
    let base = docs[0].as_ref().expect("the base document is always opened");
    let refs: Vec<&k_pdf::Document> = docs.iter().map(|d| d.as_ref().unwrap_or(base)).collect();
    // Text edits first (the form changes are numbered after them).
    base.set_overlay(text_changes);
    let mut changes = text_changes.clone();
    if !form_edits.is_empty() {
        let fields = k_pdf::form_fields(base).map_err(|e| e.to_string())?;
        let form = k_pdf::fill_form(base, &fields, form_edits).map_err(|e| e.to_string())?;
        changes.replace.extend(form.replace);
        changes.added.extend(form.added);
    }
    k_pdf::write_document_with_changes(&refs, pages, &changes, out).map_err(|e| e.to_string())
}

/// Prints pages (as for [`save_pages`]): writes them to a temporary PDF,
/// leaves out the comments if asked, and sends it to the printer.
fn print_job(
    files: &[PathBuf],
    pages: &[(usize, usize)],
    form_edits: &[(k_pdf::ObjRef, k_pdf::FieldValue)],
    text_changes: &k_pdf::Changes,
    job: &print::Job,
) -> Result<String, String> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = |part: &str| std::env::temp_dir().join(format!("k-pdf-print-{}-{n}{part}.pdf", std::process::id()));
    let (file, bare) = (temp(""), temp("-bare"));
    let result = (|| {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&file).map_err(|e| e.to_string())?);
        build_pdf(files, pages, form_edits, text_changes, &mut out)?;
        std::io::Write::flush(&mut out).map_err(|e| e.to_string())?;
        drop(out);
        let mut to_print = &file;
        if !job.settings.comments {
            let doc = k_pdf::Document::open(&file).map_err(|e| e.to_string())?;
            let count = doc.page_count().map_err(|e| e.to_string())?;
            let mut changes = k_pdf::Changes::default();
            for p in 0..count {
                k_pdf::edit::annotate::remove_comments(&doc, p, &mut changes).map_err(|e| e.to_string())?;
            }
            let all: Vec<(usize, usize)> = (0..count).map(|p| (0, p)).collect();
            let out = std::fs::File::create(&bare).map_err(|e| e.to_string())?;
            k_pdf::write_document_with_changes(&[&doc], &all, &changes, std::io::BufWriter::new(out)).map_err(|e| e.to_string())?;
            to_print = &bare;
        }
        print::send(to_print, job)
    })();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&bare);
    result
}

/// Writes `target` through `write`: into a file next to it that is then
/// renamed over it, so the target is never left half-written.
fn write_atomically(
    target: &std::path::Path,
    write: impl FnOnce(&mut std::io::BufWriter<&std::fs::File>) -> Result<(), String>,
) -> Result<(), String> {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let write = || -> Result<(), String> {
        let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut buffered = std::io::BufWriter::new(&file);
        write(&mut buffered)?;
        std::io::Write::flush(&mut buffered).map_err(|e| e.to_string())?;
        drop(buffered);
        file.sync_all().map_err(|e| e.to_string())?;
        // Keep the permissions of the file being replaced.
        if let Ok(meta) = std::fs::metadata(target) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        std::fs::rename(&tmp, target).map_err(|e| e.to_string())
    };
    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

struct Screenshot {
    path: PathBuf,
    requested: bool,
    /// Frames to wait after everything is rendered, so the UI has settled.
    settle: u32,
    /// Close the window once it is taken (`--screenshot`).
    exit: bool,
}

struct Viewer {
    ctx: egui::Context,
    pool: Pool,
    doc: Option<DocInfo>,
    loading: Option<PathBuf>,
    error: Option<String>,
    frame: u64,

    zoom: f32,
    fit: Fit,
    zoomed_at: Option<Instant>,
    textures: HashMap<usize, PageTexture>,
    failed: HashMap<usize, String>,
    /// Last render request sent, to avoid resending it every frame.
    last_want: (Vec<usize>, f32),

    /// Positioned text per page, for selection.
    chars: HashMap<usize, Arc<Vec<TextChar>>>,
    chars_requested: HashSet<usize>,
    /// Links per page (arrive together with the text).
    links: HashMap<usize, Arc<Vec<Link>>>,
    /// Scroll positions to return to after following links (Alt+←).
    back: Vec<Vec2>,
    selection: Option<Selection>,
    dragging: bool,
    /// Ctrl+A was pressed before this page's text arrived.
    select_all_pending: Option<usize>,

    current_page: usize,
    page_input: String,
    /// Scroll offset to apply on the next frame.
    pending_offset: Option<Vec2>,
    last_offset: Vec2,
    scrolling_down: bool,
    viewport_size: Vec2,

    show_info: bool,
    /// The page thumbnail sidebar.
    show_sidebar: bool,
    sidebar: sidebar::Sidebar,
    /// Pages by position, as page keys (see `worker`): the open file's
    /// pages, then pages of inserted files. Dragging thumbnails and
    /// inserting files change it; everything rendered stays keyed by page.
    order: Vec<usize>,
    /// For each page key: (document, page in that document). Document 0 is
    /// the open file, then `added` in order.
    sources: Vec<(usize, usize)>,
    /// Files opened to insert their pages.
    added: Vec<PathBuf>,
    /// Insertions waiting for their file to open: (file, position).
    inserting: Vec<(PathBuf, usize)>,
    /// Number of pages in the open file.
    base_count: usize,
    /// The open file's form fields and their (edited) values.
    form: form::Form,
    /// Edit mode: clicking text edits it.
    edit_mode: bool,
    /// Crop mode: dragging on a page chooses the area to keep.
    crop_mode: bool,
    /// Rotate mode: the panel for turning pages.
    rotate_mode: bool,
    rotate: rotate::Rotate,
    crop: crop::Crop,
    /// Redact mode: marking areas to remove for good.
    redact_mode: bool,
    redact: redact::Redact,
    /// Highlights and notes: the comment box, the Comment tool.
    comments: comments::Comments,
    /// Light, dark, or as the system is set.
    theme: egui::ThemePreference,
    /// Pages shown dark for reading (lightness turned around, hues kept).
    dark_pages: bool,
    /// The window title last set.
    window_title: String,
    /// The Compress dialog, and a compression running in the background:
    /// (size of the file as opened, report) or an error.
    compress: compress::Compress,
    compressing: Option<std::sync::mpsc::Receiver<Result<(u64, k_pdf::compress::CompressReport), String>>>,
    /// A presentation in progress, and whether presenting shows the
    /// presenter view (remembered while the app runs).
    present: Option<present::Show>,
    presenter_view: bool,
    /// The "Delete pages" dialog: the pages field, and why it can't be
    /// used if it can't.
    delete_dialog: Option<(String, Option<String>)>,
    /// The Print dialog, and a job being prepared and sent.
    print: print::Print,
    printing: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// Where the pages are shown on screen (for the crop panel).
    view_rect: Rect,
    /// Measured width of the toolbar's controls in its full (0) and
    /// compact (1) layouts, to pick the one that fits.
    toolbar_widths: [Option<f32>; 2],
    editor: editor::Editor,
    /// Earlier orders, for Ctrl+Z.
    undo: Vec<Undo>,
    /// Steps undone, for Ctrl+Shift+Z / Ctrl+Y (cleared by any new change).
    redo: Vec<Undo>,
    /// The "save changes?" question is showing; what to do afterwards.
    prompt: Option<After>,
    /// A save running in the background.
    saving: Option<Saving>,
    /// Changes were saved or discarded, so the window may close.
    allow_close: bool,
    /// Page to jump to once the document is open (from `--page`).
    start_page: Option<usize>,
    screenshot: Option<Screenshot>,
    #[cfg(feature = "script")]
    script: Option<script::Script>,
}

impl Viewer {
    fn new(cc: &eframe::CreationContext, args: Args) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install(ctx);
        ctx.set_theme(egui::ThemePreference::System);
        ctx.options_mut(|o| {
            // Ctrl +/- zooms the document, not the whole interface.
            o.zoom_with_keyboard = false;
            o.input_options.line_scroll_speed = WHEEL_SCROLL;
        });
        // A short scroll animation: smooth, but without perceptible lag.
        ctx.all_styles_mut(|s| s.scroll_animation = egui::style::ScrollAnimation::new(6000.0, egui::Rangef::new(0.0, 0.06)));

        let mut viewer = Viewer {
            ctx: ctx.clone(),
            pool: Pool::start(ctx.clone()),
            doc: None,
            loading: None,
            error: None,
            frame: 0,
            zoom: 1.0,
            fit: Fit::Width,
            zoomed_at: None,
            textures: HashMap::new(),
            failed: HashMap::new(),
            last_want: (Vec::new(), 0.0),
            chars: HashMap::new(),
            links: HashMap::new(),
            back: Vec::new(),
            chars_requested: HashSet::new(),
            selection: None,
            dragging: false,
            select_all_pending: None,
            current_page: 0,
            page_input: String::new(),
            pending_offset: None,
            last_offset: Vec2::ZERO,
            scrolling_down: true,
            viewport_size: Vec2::new(800.0, 600.0),
            show_info: false,
            show_sidebar: true,
            sidebar: sidebar::Sidebar::default(),
            order: Vec::new(),
            sources: Vec::new(),
            added: Vec::new(),
            inserting: Vec::new(),
            base_count: 0,
            form: form::Form::default(),
            edit_mode: false,
            crop_mode: false,
            rotate_mode: false,
            rotate: rotate::Rotate::default(),
            crop: crop::Crop::default(),
            redact_mode: false,
            redact: redact::Redact::default(),
            comments: comments::Comments::default(),
            compress: compress::Compress::default(),
            compressing: None,
            present: None,
            presenter_view: true,
            delete_dialog: None,
            print: print::Print::default(),
            printing: None,
            theme: egui::ThemePreference::System,
            dark_pages: false,
            window_title: "k-pdf".into(),
            view_rect: Rect::NOTHING,
            toolbar_widths: [None; 2],
            editor: editor::Editor::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            prompt: None,
            saving: None,
            allow_close: false,
            start_page: args.page.map(|p| p.saturating_sub(1)),
            screenshot: args.screenshot.map(|path| Screenshot { path, requested: false, settle: 3, exit: true }),
            #[cfg(feature = "script")]
            script: args.script.map(|p| script::Script::load(&p).unwrap_or_else(|e| panic!("script: {e}"))),
        };
        if let Some(p) = args.path {
            viewer.load(p);
        }
        viewer
    }

    /// Opens a file, first asking about unsaved changes.
    fn open(&mut self, path: PathBuf) {
        if self.is_modified() {
            self.prompt = Some(After::Open(path));
        } else {
            self.load(path);
        }
    }

    fn load(&mut self, path: PathBuf) {
        // Keep an absolute path: relative ones (from the command line) mean
        // nothing to the file dialogs, which run in another process.
        // (On Windows, canonical paths have a \\?\ prefix that file dialogs
        // and users don't expect.)
        #[cfg(windows)]
        let path = std::path::absolute(&path).unwrap_or(path);
        #[cfg(not(windows))]
        let path = std::fs::canonicalize(&path).or_else(|_| std::path::absolute(&path)).unwrap_or(path);
        self.comments.open = None;
        self.comments.placing = false;
        self.editor.open(&path);
        self.redact = redact::Redact::default();
        self.loading = Some(path.clone());
        self.error = None;
        self.pool.open(path);
    }

    /// Whether pages were moved or inserted, or form fields filled in,
    /// since the file was opened or saved.
    fn is_modified(&self) -> bool {
        self.pages_modified() || self.form.is_modified() || self.editor.is_modified()
    }

    fn pages_modified(&self) -> bool {
        self.order.len() != self.base_count || self.order.iter().enumerate().any(|(pos, &page)| pos != page)
    }

    /// The window title (shown in task bars and window switchers): the
    /// open file's name, then the application's.
    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.doc {
            Some(doc) => {
                let name = doc.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                format!("{}{name} — k-pdf", if self.is_modified() { "• " } else { "" })
            }
            None => "k-pdf".to_string(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// Treats the current state as saved (after saving, or when changes are
    /// discarded right before closing or opening another file).
    fn mark_unchanged(&mut self) {
        self.base_count = self.order.len();
        self.order = (0..self.order.len()).collect();
        self.form.mark_saved();
        self.editor.restore(k_pdf::Changes::default());
    }

    /// Asks for a PDF to insert at `position`; its pages are inserted once
    /// it has been opened (see `Reply::Added`).
    fn insert_file_dialog(&mut self, position: usize) {
        let mut dialog = rfd::FileDialog::new().add_filter("PDF documents", &["pdf", "PDF"]);
        if let Some(dir) = self.doc.as_ref().and_then(|d| d.path.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.error = None;
            self.inserting.push((path.clone(), position));
            self.pool.add_document(path);
        }
    }

    fn take_insert_position(&mut self, path: &PathBuf) -> Option<usize> {
        let i = self.inserting.iter().position(|(p, _)| p == path)?;
        Some(self.inserting.remove(i).1)
    }

    /// Position of a page in the current order.
    fn position_of(&self, page: usize) -> usize {
        self.order.iter().position(|&p| p == page).unwrap_or(page)
    }

    /// Deletes the pages at `positions` (one undo step). At least one page
    /// stays.
    fn delete_pages(&mut self, positions: &[usize]) {
        let mut positions: Vec<usize> = positions.iter().copied().filter(|&p| p < self.order.len()).collect();
        positions.sort_unstable();
        positions.dedup();
        if positions.is_empty() {
            return;
        }
        if positions.len() >= self.order.len() {
            self.error = Some("Can't delete every page: a PDF needs at least one page.".into());
            return;
        }
        self.close_comment();
        let viewing = self.order.get(self.current_page).copied();
        let before_current = positions.iter().filter(|&&p| p < self.current_page).count();
        self.record(Undo::Order(self.order.clone()));
        for &p in positions.iter().rev() {
            self.order.remove(p);
        }
        self.selection = None;
        self.back.clear();
        // Stay on the page being read, or the one now in its place.
        let target = viewing
            .and_then(|k| self.order.iter().position(|&x| x == k))
            .unwrap_or(self.current_page.saturating_sub(before_current))
            .min(self.order.len() - 1);
        self.go_to_page(target);
    }

    /// The "Delete pages" dialog: a list or range of pages.
    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let Some((text, error)) = &mut self.delete_dialog else { return };
        let count = self.order.len();
        let parsed = crop::parse_pages(text, count);
        let mut delete = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("delete-pages")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Delete pages");
            ui.add_space(8.0);
            ui.label("Pages to delete:");
            let field = ui.add(egui::TextEdit::singleline(text).hint_text("e.g. 2, 5-7").desired_width(f32::INFINITY));
            field.request_focus();
            let p = theme::palette(ui);
            match (&parsed, error.as_ref()) {
                (_, Some(e)) => {
                    ui.colored_label(p.danger, e.as_str());
                }
                (Ok(pages), None) if pages.len() >= count => {
                    ui.colored_label(p.danger, "That's every page: a PDF needs at least one.");
                }
                (Ok(pages), None) => {
                    let n = pages.len();
                    ui.label(egui::RichText::new(format!("Deletes {n} of {count} {}. Ctrl+Z brings them back until you save.", if count == 1 { "page" } else { "pages" })).color(p.weak));
                }
                (Err(e), None) if !text.trim().is_empty() => {
                    ui.colored_label(p.weak, e.as_str());
                }
                _ => {}
            }
            ui.add_space(10.0);
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                let ok = matches!(&parsed, Ok(pages) if pages.len() < count);
                if theme::danger_button(ui, "Delete", ok).clicked() || (ok && enter) {
                    delete = parsed.clone().ok();
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
            if field.changed() {
                *error = None;
            }
        });
        if let Some(positions) = delete {
            self.delete_dialog = None;
            self.delete_pages(&positions);
        } else if close || modal.should_close() {
            self.delete_dialog = None;
        }
    }

    fn move_page(&mut self, from: usize, to: usize) {
        // Keep showing the page that was being read.
        let viewing = self.order.get(self.current_page).copied();
        self.record(Undo::Order(self.order.clone()));
        sidebar::move_item(&mut self.order, from, to);
        self.back.clear();
        if let Some(page) = viewing {
            self.go_to_page(self.position_of(page));
        }
    }

    /// Remembers the state before a change, for undo. A new change ends
    /// what can be redone.
    fn record(&mut self, step: Undo) {
        self.undo.push(step);
        self.redo.clear();
    }

    fn undo(&mut self) {
        self.close_comment();
        if let Some(step) = self.undo.pop() {
            let current = self.apply_step(step);
            self.redo.push(current);
        }
    }

    fn redo(&mut self) {
        self.close_comment();
        if let Some(step) = self.redo.pop() {
            let current = self.apply_step(step);
            self.undo.push(current);
        }
    }

    /// Goes back (or forward) to a remembered state; returns the state it
    /// replaced, for going the other way.
    fn apply_step(&mut self, step: Undo) -> Undo {
        match step {
            Undo::Order(order) => {
                let viewing = self.order.get(self.current_page).copied();
                let current = std::mem::replace(&mut self.order, order);
                self.back.clear();
                if let Some(page) = viewing {
                    self.go_to_page(self.position_of(page));
                }
                Undo::Order(current)
            }
            Undo::Text(changes) => {
                let current = self.editor.changes().clone();
                self.editor.restore(changes);
                self.content_changed();
                Undo::Text(current)
            }
        }
    }

    /// Re-renders a page of the open file after a text edit (`None`: all).
    fn refresh_pages(&mut self, page: Option<usize>) {
        let keys: Vec<usize> = (0..self.sources.len())
            .filter(|&k| self.sources[k].0 == 0 && page.is_none_or(|p| self.sources[k].1 == p))
            .collect();
        for key in keys {
            // Keep showing the old bitmap until the new one arrives.
            if let Some(t) = self.textures.get_mut(&key) {
                t.scale = -1.0;
            }
            self.failed.remove(&key);
            self.chars.remove(&key);
            self.links.remove(&key);
            self.chars_requested.remove(&key);
            self.sidebar.invalidate(key);
        }
        if self.selection.is_some_and(|s| page.is_none_or(|p| self.sources.get(s.page) == Some(&(0, p)))) {
            self.selection = None;
        }
        self.last_want = (Vec::new(), 0.0);
    }

    /// After crops (or undoing them): page sizes, form field positions and
    /// every page's rendering change.
    fn content_changed(&mut self) {
        self.comments.invalidate();
        self.pool.set_overlay(Arc::new(self.editor.changes().clone()));
        let mut resized = Vec::new();
        if let (Some(sizes), Some(doc)) = (self.editor.page_sizes(), self.doc.as_mut()) {
            for (key, &(d, p)) in self.sources.iter().enumerate() {
                if d == 0
                    && let (Some(size), Some(slot)) = (sizes.get(p), doc.page_sizes.get_mut(key))
                    && *slot != *size
                {
                    *slot = *size;
                    resized.push(key);
                }
            }
        }
        self.form.update_layout(&self.editor.form_fields());
        self.refresh_pages(None);
        // Stay on the same page as sizes change.
        if !resized.is_empty() {
            self.go_to_page(self.current_page);
        }
        // The old bitmaps of resized pages would show stretched: drop them.
        for key in resized {
            self.textures.remove(&key);
        }
    }

    fn handle_crop_action(&mut self, action: crop::Action) {
        let base_pages: Vec<usize> = (0..self.base_count).collect();
        let result = match action {
            crop::Action::Page { page, rect } => self.editor.crop(&[page], Some(rect)),
            crop::Action::All { rect } => self.editor.crop(&base_pages, Some(rect)),
            crop::Action::Pages { positions, rect } => {
                // Page numbers as shown → pages of the open file (pages
                // inserted from other files can't be cropped yet).
                let pages: Vec<usize> = positions
                    .iter()
                    .filter_map(|&pos| self.order.get(pos).and_then(|&key| self.sources.get(key)))
                    .filter(|(d, _)| *d == 0)
                    .map(|&(_, p)| p)
                    .collect();
                if pages.is_empty() {
                    self.error = Some("Those pages come from inserted files and can't be cropped yet.".into());
                    return;
                }
                self.editor.crop(&pages, Some(rect))
            }
            crop::Action::ResetPage { page } => self.editor.crop(&[page], None),
            crop::Action::ResetAll => self.editor.crop(&base_pages, None),
            crop::Action::Done => {
                self.crop_mode = false;
                return;
            }
        };
        match result {
            Ok(before) => {
                self.record(Undo::Text(before));
                self.content_changed();
            }
            Err(e) => self.error = Some(format!("Could not crop: {e}")),
        }
    }

    fn set_crop_mode(&mut self, on: bool) {
        if on {
            self.rotate_mode = false;
            self.close_comment();
            self.comments.placing = false;
            self.set_edit_mode(false);
            self.redact_mode = false;
        }
        self.crop_mode = on;
        self.crop.clear();
        self.selection = None;
    }

    /// Rotate mode shows the rotate panel; the pages stay usable.
    fn set_rotate_mode(&mut self, on: bool) {
        if on {
            self.close_comment();
            self.comments.placing = false;
            self.set_edit_mode(false);
            self.set_crop_mode(false);
            self.redact_mode = false;
        }
        self.rotate_mode = on;
    }

    /// Turns the pages at `positions` (in the view) by `degrees`
    /// clockwise, as one undo step.
    fn rotate_pages(&mut self, positions: &[usize], degrees: i32) {
        // Positions → pages of the open file (pages inserted from other
        // files can't be rotated until the file is saved and reopened).
        let pages: Vec<usize> = positions
            .iter()
            .filter_map(|&pos| self.order.get(pos).and_then(|&key| self.sources.get(key)))
            .filter(|(d, _)| *d == 0)
            .map(|&(_, p)| p)
            .collect();
        if pages.is_empty() {
            self.error = Some("Pages inserted from other files can't be rotated until the file is saved.".into());
            return;
        }
        match self.editor.rotate(&pages, degrees) {
            Ok(before) => {
                self.record(Undo::Text(before));
                self.content_changed();
            }
            Err(e) => self.error = Some(format!("Could not rotate: {e}")),
        }
    }

    /// Shows pages dark for reading, or normally again: every page and
    /// thumbnail is rendered again (the old ones stay until then).
    fn set_dark_pages(&mut self, dark: bool) {
        if dark == self.dark_pages {
            return;
        }
        self.dark_pages = dark;
        theme::set_dark_pages(dark);
        self.pool.set_dark_pages(dark);
        for key in 0..self.sources.len() {
            if let Some(t) = self.textures.get_mut(&key) {
                t.scale = -1.0;
            }
            self.failed.remove(&key);
            self.sidebar.invalidate(key);
        }
        self.last_want = (Vec::new(), 0.0);
    }

    /// Redact mode keeps its marks while off, until they are applied or
    /// another file is opened.
    fn set_redact_mode(&mut self, on: bool) {
        if on {
            self.rotate_mode = false;
            self.close_comment();
            self.comments.placing = false;
            // Before anything clears the selection.
            self.mark_selection_for_redaction();
            self.set_edit_mode(false);
            self.crop_mode = false;
            self.crop.clear();
        }
        self.redact_mode = on;
        self.selection = None;
    }

    /// Turning redact mode on with text selected: the selected text is
    /// marked (each line's area), ready to apply.
    fn mark_selection_for_redaction(&mut self) {
        let Some(sel) = self.selection.filter(|s| !s.is_empty()) else { return };
        let Some(&(0, page)) = self.sources.get(sel.page) else {
            self.redact.message = Some("pages inserted from other files can't be redacted until the file is saved".into());
            return;
        };
        let Some(chars) = self.chars.get(&sel.page) else { return };
        let marks: Vec<redact::Mark> = selection::highlight_rects(chars, sel.range())
            .into_iter()
            // A little room, so nothing of the glyphs shows.
            .map(|rect| redact::Mark { page, rect: rect.expand(0.5) })
            .collect();
        let lines = marks.len();
        let new = self.redact.add(marks);
        self.redact.message = Some(match (lines, new) {
            (0, _) => return,
            (_, 0) => "the selected text was already marked".into(),
            (1, _) => "marked the selected text — Apply redacts it".into(),
            (n, _) => format!("marked the selected text ({n} lines) — Apply redacts it"),
        });
    }

    fn handle_redact_action(&mut self, action: redact::Action) {
        match action {
            redact::Action::Apply => {
                let marks: Vec<(usize, [f32; 4])> = self
                    .redact
                    .marks
                    .iter()
                    .map(|m| (m.page, [m.rect.left(), m.rect.top(), m.rect.right(), m.rect.bottom()]))
                    .collect();
                match self.editor.redact(&marks) {
                    Ok((before, done)) => {
                        self.record(Undo::Text(before));
                        self.content_changed();
                        self.redact.marks.clear();
                        self.redact.message = Some(redaction_summary(&done));
                    }
                    Err(e) => self.error = Some(format!("Could not redact: {e}")),
                }
            }
            redact::Action::Find(words) => {
                let found = self.editor.find_text(&words);
                let pages: HashSet<usize> = found.iter().map(|&(p, _)| p).collect();
                let new = self.redact.add(found.iter().map(|&(page, [l, t, r, b])| redact::Mark {
                    page,
                    // A little room, so nothing of the glyphs shows.
                    rect: Rect::from_min_max(Pos2::new(l, t), Pos2::new(r, b)).expand(0.5),
                }));
                self.redact.message = Some(match (found.len(), new) {
                    (0, _) => format!("\"{words}\" wasn't found"),
                    (n, 0) => format!("all {n} places were already marked"),
                    (n, _) => format!(
                        "marked {n} {} on {} {}",
                        if n == 1 { "place" } else { "places" },
                        pages.len(),
                        if pages.len() == 1 { "page" } else { "pages" }
                    ),
                });
                // Show the first match.
                if let Some(&(page, _)) = found.first()
                    && let Some(key) = self.sources.iter().position(|&s| s == (0, page))
                {
                    self.go_to_page(self.position_of(key));
                }
            }
            redact::Action::Done => self.redact_mode = false,
        }
    }

    fn handle_editor_event(&mut self, event: editor::Event) {
        match event {
            editor::Event::Applied { page, before } => {
                self.record(Undo::Text(before));
                self.pool.set_overlay(Arc::new(self.editor.changes().clone()));
                self.refresh_pages(Some(page));
            }
            editor::Event::Failed(e) => self.error = Some(format!("Could not change the text: {e}")),
        }
    }

    fn set_edit_mode(&mut self, on: bool) {
        if !on && let Some(event) = self.editor.commit() {
            self.handle_editor_event(event);
        }
        if on {
            self.close_comment();
            self.comments.placing = false;
            self.crop_mode = false;
            self.crop.clear();
            self.redact_mode = false;
            self.rotate_mode = false;
        }
        self.edit_mode = on;
        self.selection = None;
    }

    // ------------------------------------------------------------------
    // Comments
    // ------------------------------------------------------------------

    /// The Comment tool: the next click on a page places a note there.
    fn set_placing_note(&mut self, on: bool) {
        if on {
            self.rotate_mode = false;
            self.close_comment();
            self.set_edit_mode(false);
            self.set_crop_mode(false);
            self.redact_mode = false;
        }
        self.comments.placing = on;
    }

    /// After adding, changing or deleting a comment on `page` (of the open
    /// file): that page is drawn again and the lists read again.
    fn annotations_changed(&mut self, page: usize) {
        self.comments.invalidate();
        self.pool.set_overlay(Arc::new(self.editor.changes().clone()));
        self.refresh_pages(Some(page));
    }

    /// Highlights the selected text in `color`; with `comment`, also opens
    /// the comment box for it.
    fn highlight_selection(&mut self, color: [u8; 3], comment: bool) {
        let Some(sel) = self.selection.filter(|s| !s.is_empty()) else { return };
        let Some(&(0, page)) = self.sources.get(sel.page) else {
            self.error = Some("Pages inserted from other files can't be highlighted yet.".into());
            return;
        };
        let Some(chars) = self.chars.get(&sel.page) else { return };
        let areas: Vec<[f32; 4]> = selection::highlight_rects(chars, sel.range())
            .iter()
            .map(|r| [r.left(), r.top(), r.right(), r.bottom()])
            .collect();
        // Commenting on text that is already highlighted: that highlight's
        // comment, rather than a second highlight.
        if comment {
            let existing = self.comments.on_page(&self.editor, page).iter().rev().find(|c| {
                c.kind == k_pdf::edit::annotate::CommentKind::Highlight
                    && areas.iter().all(|&[l, t, r, b]| {
                        let (x, y) = ((l + r) / 2.0, (t + b) / 2.0);
                        c.areas.iter().any(|a| x >= a[0] - 1.0 && x <= a[2] + 1.0 && y >= a[1] - 1.0 && y <= a[3] + 1.0)
                    })
            }).cloned();
            if let Some(c) = existing {
                self.selection = None;
                self.close_comment();
                let before = self.editor.changes().clone();
                self.comments.start(&c, before, false, true);
                return;
            }
        }
        let author = comments::author();
        let added = self.editor.annotate(|doc, changes| {
            k_pdf::edit::annotate::add_highlight(doc, page, &areas, color, "", &author, changes)
        });
        match added {
            Ok((before, id)) => {
                self.comments.color = color;
                self.annotations_changed(page);
                self.selection = None;
                match self.comments.find(&self.editor, page, id).filter(|_| comment) {
                    Some(c) => self.comments.start(&c, before, true, true),
                    None => self.record(Undo::Text(before)),
                }
            }
            Err(e) => self.error = Some(format!("Could not highlight: {e}")),
        }
    }

    /// Places a note at `at` (display points) on `page` of the open file
    /// and opens its comment box.
    fn place_note(&mut self, page: usize, at: Pos2) {
        self.comments.placing = false;
        let author = comments::author();
        let color = k_pdf::edit::annotate::HIGHLIGHT_YELLOW;
        let added = self.editor.annotate(|doc, changes| {
            k_pdf::edit::annotate::add_note(doc, page, [at.x - 3.0, at.y - 3.0], color, "", &author, changes)
        });
        match added {
            Ok((before, id)) => {
                self.annotations_changed(page);
                if let Some(c) = self.comments.find(&self.editor, page, id) {
                    self.comments.start(&c, before, true, true);
                } else {
                    self.record(Undo::Text(before));
                }
            }
            Err(e) => self.error = Some(format!("Could not add the note: {e}")),
        }
    }

    /// Opens the comment box of `c` (closing any other).
    fn open_comment(&mut self, c: &k_pdf::edit::annotate::Comment, focus: bool) {
        if self.comments.open.as_ref().is_some_and(|o| o.id == c.id) {
            return;
        }
        self.close_comment();
        let before = self.editor.changes().clone();
        self.comments.start(c, before, false, focus);
    }

    /// Closes the comment box, keeping what was typed (one undo step for
    /// the whole visit). A new note left empty is removed again.
    fn close_comment(&mut self) {
        let Some(mut open) = self.comments.open.take() else { return };
        let trimmed = open.text.trim_end().to_string();
        if trimmed != open.text {
            open.text = trimmed;
            self.set_comment_text(&open);
        }
        let note = self.comments.find(&self.editor, open.page, open.id).map(|c| c.kind);
        if open.created && open.text.is_empty() && note == Some(k_pdf::edit::annotate::CommentKind::Note) {
            self.editor.restore(open.before);
            self.annotations_changed(open.page);
            return;
        }
        if open.created || open.changed() {
            self.record(Undo::Text(open.before));
            self.pool.set_overlay(Arc::new(self.editor.changes().clone()));
        }
    }

    /// Deletes the comment whose box is open.
    fn delete_open_comment(&mut self) {
        let Some(open) = self.comments.open.take() else { return };
        if open.created {
            // Just made: as if it never was.
            self.editor.restore(open.before);
        } else {
            let (page, id) = (open.page, open.id);
            match self.editor.annotate(|doc, changes| k_pdf::edit::annotate::delete_comment(doc, page, id, changes)) {
                Ok(_) => self.record(Undo::Text(open.before)),
                Err(e) => self.error = Some(format!("Could not delete the comment: {e}")),
            }
        }
        self.annotations_changed(open.page);
    }

    /// Stores the text typed in the comment box (as it is typed).
    fn set_comment_text(&mut self, open: &comments::Open) {
        let (id, text) = (open.id, open.text.clone());
        match self.editor.annotate(|doc, changes| k_pdf::edit::annotate::set_comment_text(doc, id, &text, changes)) {
            Ok(_) => self.comments.update_text(id, &text),
            Err(e) => self.error = Some(format!("Could not change the comment: {e}")),
        }
    }

    /// The comments for the sidebar's list, in page order.
    fn comment_rows(&mut self) -> Vec<sidebar::CommentRow> {
        let all = self.comments.all(&self.editor);
        let mut rows = Vec::with_capacity(all.len());
        let shown: HashSet<usize> = self.order.iter().copied().collect();
        for c in all.iter().filter(|c| shown.contains(&c.page)) {
            // Pages of the open file have the same key as their index.
            let chars = self.chars.get(&c.page).cloned();
            let excerpt = self.comments.excerpt(c, chars.as_deref().map(|v| v.as_slice()));
            if excerpt.is_none() && c.kind == k_pdf::edit::annotate::CommentKind::Highlight {
                self.request_chars(c.page);
            }
            let mut by = c.author.clone();
            if let Some(d) = &c.modified {
                if !by.is_empty() {
                    by += " · ";
                }
                by += &comments::short_date(d);
            }
            rows.push(sidebar::CommentRow {
                id: c.id,
                page: c.page,
                position: self.position_of(c.page),
                color: comments::color32(c.color),
                kind: comments::kind_name(&c.kind),
                excerpt,
                text: c.text.clone(),
                by,
            });
        }
        // Pages may have been moved.
        rows.sort_by_key(|r| r.position);
        rows
    }

    /// The comment box and the bar over selected text, on top of the pages.
    fn comment_overlays(&mut self, ctx: &egui::Context, anchors: &CommentAnchors) {
        let view = self.view_rect;
        if let Some(open) = &self.comments.open {
            let (page, id) = (open.page, open.id);
            let Some(comment) = self.comments.find(&self.editor, page, id) else {
                self.comments.open = None;
                return;
            };
            let mut open = self.comments.open.take().expect("checked above");
            let action = comments::popup(ctx, &mut open, &comment, anchors.open, view);
            self.comments.open = Some(open);
            match action {
                comments::PopupAction::None => {}
                comments::PopupAction::Edited => {
                    let open = self.comments.open.take().expect("still open");
                    self.set_comment_text(&open);
                    self.comments.open = Some(open);
                }
                comments::PopupAction::Close => self.close_comment(),
                comments::PopupAction::Delete => self.delete_open_comment(),
            }
        } else if let Some(anchor) = anchors.selection
            && let Some(action) = comments::selection_bar(ctx, anchor, view, self.comments.color)
        {
            match action {
                comments::BarAction::Highlight(color) => self.highlight_selection(color, false),
                comments::BarAction::Comment => self.highlight_selection(self.comments.color, true),
            }
        }
    }

    /// Saves the current page order to `target` in the background, then
    /// does `after`.
    fn save(&mut self, target: PathBuf, after: After) {
        self.close_comment();
        let Some(doc) = &self.doc else { return };
        if self.saving.is_some() {
            return;
        }
        if doc.encrypted {
            self.error = Some("Encrypted files can't be saved yet.".into());
            return;
        }
        let mut files = vec![doc.path.clone()];
        files.extend(self.added.iter().cloned());
        let pages: Vec<(usize, usize)> = self.order.iter().map(|&key| self.sources[key]).collect();
        let form_edits = self.form.edits();
        let text_changes = self.editor.changes().clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        let dest = target.clone();
        std::thread::spawn(move || {
            let _ = tx.send(save_pages(&files, &pages, &form_edits, &text_changes, &dest));
            ctx.request_repaint();
        });
        self.error = None;
        self.saving = Some(Saving { target, result: rx, after });
    }

    fn save_as_dialog(&mut self, after: After) {
        let Some(doc) = &self.doc else { return };
        let name = doc.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut dialog = rfd::FileDialog::new().add_filter("PDF documents", &["pdf"]).set_file_name(name);
        if let Some(dir) = doc.path.parent() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.save_file() {
            self.save(path, after);
        }
    }

    /// Saves over the open file (Ctrl+S).
    fn save_in_place(&mut self, after: After) {
        if let Some(path) = self.doc.as_ref().map(|d| d.path.clone()) {
            self.save(path, after);
        }
    }

    // ------------------------------------------------------------------
    // Presenting
    // ------------------------------------------------------------------

    /// Starts presenting, from the first page or the current one.
    fn start_presentation(&mut self, ctx: &egui::Context, from_current: bool) {
        if self.doc.is_none() || self.page_count() == 0 {
            return;
        }
        self.close_comment();
        self.set_edit_mode(false);
        self.set_crop_mode(false);
        self.redact_mode = false;
        self.rotate_mode = false;
        self.comments.placing = false;
        self.selection = None;
        let position = if from_current { self.current_page } else { 0 };
        // Full screen once the screens are known (next frame).
        self.present = Some(present::Show::new(position, self.presenter_view, self.zoom));
        ctx.request_repaint();
    }

    fn end_presentation(&mut self, ctx: &egui::Context, show: present::Show) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        self.zoom = show.saved_zoom;
        self.last_want = (Vec::new(), 0.0);
        // Back where the presentation ended.
        self.go_to_page(show.position);
    }

    /// A frame of the presentation: the slide full screen, the presenter
    /// view if it is on, and what the keys, clicks and buttons asked for.
    fn presentation(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let ctx = ui.ctx().clone();
        let Some(doc) = &self.doc else { return };
        let Some(mut show) = self.present.take() else { return };
        // First frame: with several screens, the slides go on one and the
        // presenter view on the main one; with one, full screen.
        if !show.screens_read && !show.set_screens(screens(frame)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        if show.move_slides {
            show.move_slides = false;
            if let Some(screen) = show.slides_screen.and_then(|i| show.screens.get(i)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::SetMonitor(screen.index));
            }
        }
        let sizes: Vec<Vec2> = self.order.iter().map(|&k| doc.page_sizes[k]).collect();
        let count = sizes.len();
        show.position = show.position.min(count.saturating_sub(1));
        let title = doc.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut navs = ctx.input(|i| show.keys(i));

        // Speaker notes: the comments on the page.
        let notes = match self.sources.get(self.order[show.position]) {
            Some(&(0, page)) if show.presenter => self
                .comments
                .on_page(&self.editor, page)
                .iter()
                .filter(|c| !c.text.trim().is_empty())
                .map(|c| c.text.trim().to_string())
                .collect(),
            _ => Vec::new(),
        };

        {
            let (textures, order) = (&self.textures, &self.order);
            let texture = |pos: usize| order.get(pos).and_then(|k| textures.get(k)).map(|t| t.texture.id());
            egui::CentralPanel::no_frame().show(ui, |ui| navs.extend(present::audience(ui, &mut show, &sizes, &texture)));
            if show.presenter {
                let notes = present::Notes { title, notes };
                navs.extend(present::presenter(&ctx, &mut show, &sizes, &texture, &notes));
            }
        }

        // Render the slide (and its neighbors) to fill the screen.
        let screen = ctx.content_rect().size();
        let size = sizes[show.position];
        self.zoom = (screen.x / size.x.max(1.0)).min(screen.y / size.y.max(1.0));
        for pos in [show.position, show.position + 1] {
            if let Some(t) = self.order.get(pos).and_then(|k| self.textures.get_mut(k)) {
                t.last_used = self.frame;
            }
        }
        self.scrolling_down = true;
        self.request_pages(&ctx, &[show.position], ctx.pixels_per_point());

        let mut exit = false;
        for nav in navs {
            match nav {
                present::Nav::Next => show.go(show.position + 1, count),
                present::Nav::Prev => show.go(show.position.saturating_sub(1), count),
                present::Nav::First => show.go(0, count),
                present::Nav::Last => show.go(count - 1, count),
                present::Nav::GoTo(p) => show.go(p, count),
                present::Nav::Blank(color) => show.blank = if show.blank == Some(color) { None } else { Some(color) },
                present::Nav::TogglePresenter => {
                    show.presenter = !show.presenter;
                    self.presenter_view = show.presenter;
                    // Turned on with several screens: the slides move to
                    // another one.
                    if show.presenter && show.slides_screen.is_none() && show.screens.len() >= 2 {
                        show.split();
                    }
                }
                present::Nav::SlidesOn(screen) => show.place(screen),
                present::Nav::SwapScreens => show.swap_screens(),
                present::Nav::Exit => exit = true,
            }
        }
        if exit {
            self.end_presentation(&ctx, show);
        } else {
            self.present = Some(show);
        }
    }

    /// Opens the Print dialog (what is being typed in a comment is kept).
    fn open_print_dialog(&mut self, ctx: &egui::Context) {
        if self.doc.is_none() {
            return;
        }
        self.close_comment();
        self.print.open(ctx);
    }

    /// Writes the pages to print (with the unsaved changes, and without
    /// comments if they are left out) to a temporary file and sends it, in
    /// the background.
    fn start_print(&mut self, job: print::Job) {
        let Some(doc) = &self.doc else { return };
        if doc.encrypted {
            self.print.state = print::State::Done(Err("encrypted files can't be printed yet".into()));
            return;
        }
        let mut files = vec![doc.path.clone()];
        files.extend(self.added.iter().cloned());
        let pages: Vec<(usize, usize)> = job.positions.iter().map(|&pos| self.sources[self.order[pos]]).collect();
        let form_edits = self.form.edits();
        let text_changes = self.editor.changes().clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = print_job(&files, &pages, &form_edits, &text_changes, &job);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.printing = Some(rx);
    }

    fn poll_print(&mut self) {
        let Some(rx) = &self.printing else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.printing = None;
        self.print.state = print::State::Done(result);
    }

    /// Asks where to save a compressed copy of the current state (with
    /// unsaved changes), then compresses in the background.
    fn start_compress(&mut self, options: k_pdf::compress::CompressOptions) {
        self.close_comment();
        let Some(doc) = &self.doc else { return };
        if doc.encrypted {
            self.error = Some("Encrypted files can't be compressed yet.".into());
            return;
        }
        let stem = doc.path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PDF documents", &["pdf"])
            .set_file_name(format!("{stem}-compressed.pdf"));
        if let Some(dir) = doc.path.parent() {
            dialog = dialog.set_directory(dir);
        }
        let Some(target) = dialog.save_file() else {
            self.compress.open(); // back to the options
            return;
        };
        let mut files = vec![doc.path.clone()];
        files.extend(self.added.iter().cloned());
        let pages: Vec<(usize, usize)> = self.order.iter().map(|&key| self.sources[key]).collect();
        let form_edits = self.form.edits();
        let text_changes = self.editor.changes().clone();
        let before = std::fs::metadata(&doc.path).map(|m| m.len()).unwrap_or(0);
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        let dest = target.clone();
        std::thread::spawn(move || {
            let run = || -> Result<(u64, k_pdf::compress::CompressReport), String> {
                // The current state, as it would be saved, then compressed.
                let mut current = Vec::new();
                build_pdf(&files, &pages, &form_edits, &text_changes, &mut current)?;
                let doc = k_pdf::Document::from_bytes(current).map_err(|e| e.to_string())?;
                let mut report = None;
                write_atomically(&dest, |out| {
                    report = Some(k_pdf::compress::compress(&doc, &options, out).map_err(|e| e.to_string())?);
                    Ok(())
                })?;
                Ok((before, report.expect("set when writing succeeded")))
            };
            let _ = tx.send(run());
            ctx.request_repaint();
        });
        self.error = None;
        self.compress.state = compress::State::Working(target);
        self.compressing = Some(rx);
    }

    fn poll_compress(&mut self) {
        let Some(rx) = &self.compressing else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.compressing = None;
        let compress::State::Working(target) = std::mem::replace(&mut self.compress.state, compress::State::Closed) else {
            return;
        };
        match result {
            Ok((before, report)) => {
                let brotli = self.compress.brotli_chosen();
                // Compressed over the open file: show it as it is now.
                let same = self.doc.as_ref().is_some_and(|d| {
                    std::fs::canonicalize(&d.path).ok() == std::fs::canonicalize(&target).ok()
                });
                if same {
                    self.undo.clear();
                    self.redo.clear();
                    self.mark_unchanged();
                    self.start_page = Some(self.current_page);
                    self.load(target.clone());
                }
                self.compress.state = compress::State::Done { target, before, report, brotli };
            }
            Err(e) => self.error = Some(format!("Could not compress: {e}")),
        }
    }

    /// Carries out what was waiting for the changes to be saved or dropped.
    fn proceed(&mut self, ctx: &egui::Context, after: After) {
        match after {
            After::Nothing => {}
            After::Close => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            After::Open(path) => self.load(path),
        }
    }

    fn poll_save(&mut self, ctx: &egui::Context) {
        let Some(saving) = &self.saving else { return };
        let Ok(result) = saving.result.try_recv() else { return };
        let Saving { target, after, .. } = self.saving.take().unwrap();
        match result {
            Ok(()) => {
                // Nothing is unsaved any more.
                self.undo.clear();
                self.redo.clear();
                self.mark_unchanged();
                match after {
                    // Reopen the saved file (with its new pages), at the same page.
                    After::Nothing => {
                        self.start_page = Some(self.current_page);
                        self.load(target);
                    }
                    after => self.proceed(ctx, after),
                }
            }
            Err(e) => self.error = Some(format!("Could not save {}: {e}", target.display())),
        }
    }

    /// The "save changes?" dialog.
    fn unsaved_prompt(&mut self, ctx: &egui::Context) {
        let Some(after) = &self.prompt else { return };
        let name = self
            .doc
            .as_ref()
            .and_then(|d| d.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let when = match after {
            After::Open(_) => "opening another file",
            _ => "closing",
        };
        #[derive(PartialEq)]
        enum Choice {
            Save,
            Discard,
            Cancel,
        }
        let mut choice = None;
        let response = egui::Modal::new(egui::Id::new("unsaved-changes")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading("Save changes?");
            ui.add_space(6.0);
            ui.label(format!("\"{name}\" has unsaved changes. Save them before {when}?"));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, "Save").clicked() {
                    choice = Some(Choice::Save);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(Choice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(Choice::Cancel);
                }
            });
        });
        if response.should_close() && choice.is_none() {
            choice = Some(Choice::Cancel); // Esc or a click outside
        }
        let Some(choice) = choice else { return };
        let after = self.prompt.take().unwrap_or(After::Nothing);
        match choice {
            Choice::Save => self.save_in_place(after),
            Choice::Discard => {
                self.undo.clear();
                self.redo.clear();
                self.mark_unchanged();
                self.proceed(ctx, after);
            }
            Choice::Cancel => {}
        }
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("PDF documents", &["pdf", "PDF"]).pick_file() {
            self.open(path);
        }
    }

    fn handle_replies(&mut self, ctx: &egui::Context) {
        for reply in self.pool.poll() {
            match reply {
                Reply::Opened(mut info) => {
                    let page_count = info.page_sizes.len();
                    self.form = form::Form::new(std::mem::take(&mut info.fields));
                    let outline = std::mem::take(&mut info.outline);
                    // Reopened after saving: the sidebar stays as it was.
                    let view = self.doc.as_ref().filter(|d| d.path == info.path).map(|_| self.sidebar.view());
                    self.doc = Some(*info);
                    self.loading = None;
                    self.textures.clear();
                    self.failed.clear();
                    self.chars.clear();
                    self.chars_requested.clear();
                    self.links.clear();
                    self.sidebar.clear();
                    self.sidebar.set_outline(outline, page_count, view);
                    self.comments.reset();
                    self.back.clear();
                    self.order = (0..page_count).collect();
                    self.sources = (0..page_count).map(|p| (0, p)).collect();
                    self.added.clear();
                    self.inserting.clear();
                    self.base_count = page_count;
                    self.undo.clear();
                    self.redo.clear();
                    self.selection = None;
                    self.last_want = (Vec::new(), 0.0);
                    self.current_page = 0;
                    self.page_input = "1".into();
                    self.pending_offset = Some(Vec2::ZERO);
                    self.fit = Fit::Width;
                    if let Some(p) = self.start_page.take() {
                        self.go_to_page(p);
                    }
                }
                Reply::OpenFailed(path, e) => {
                    self.loading = None;
                    self.error = Some(format!("Could not open {}: {e}", path.display()));
                }
                // Rendered before dark pages were switched on or off.
                Reply::Page { dark, .. } | Reply::Thumb { dark, .. } if dark != self.dark_pages => {}
                Reply::Page { page, scale, image, .. } => {
                    let bytes = image.width() * image.height() * 4;
                    let texture = ctx.load_texture(format!("page-{page}"), image, egui::TextureOptions::LINEAR);
                    self.textures.insert(page, PageTexture { texture, scale, bytes, last_used: self.frame });
                    if let Some(&(0, p)) = self.sources.get(page) {
                        self.editor.page_rendered(p);
                    }
                    self.failed.remove(&page);
                }
                Reply::Thumb { page, image, .. } => self.sidebar.insert(ctx, page, image),
                Reply::Added { path, first, page_sizes } => {
                    self.added.push(path.clone());
                    let d = self.added.len();
                    let position = self.take_insert_position(&path);
                    if let Some(doc) = &mut self.doc
                        && first == doc.page_sizes.len()
                    {
                        let n = page_sizes.len();
                        doc.page_sizes.extend(page_sizes);
                        self.sources.extend((0..n).map(|p| (d, p)));
                        if let Some(pos) = position
                            && n > 0
                        {
                            self.record(Undo::Order(self.order.clone()));
                            let pos = pos.min(self.order.len());
                            self.order.splice(pos..pos, first..first + n);
                            self.back.clear();
                            // Show the inserted pages.
                            self.go_to_page(pos);
                        }
                    }
                }
                Reply::AddFailed(path, e) => {
                    // Keep document numbers in step with the workers.
                    self.added.push(path.clone());
                    self.take_insert_position(&path);
                    self.error = Some(format!("Could not insert {}: {e}", path.display()));
                }
                Reply::PageFailed { page, error } => {
                    self.failed.insert(page, error);
                }
                Reply::Chars { page, chars, links } => {
                    self.links.insert(page, links);
                    if self.select_all_pending == Some(page) {
                        self.select_all_pending = None;
                        self.selection = Some(Selection { page, anchor: 0, focus: chars.len() });
                    }
                    self.chars.insert(page, chars);
                }
            }
        }
    }

    /// Number of pages as currently arranged.
    fn page_count(&self) -> usize {
        self.order.len()
    }

    /// Top edge of each page in content coordinates (points), plus the
    /// total content size.
    fn layout(&self) -> (Vec<f32>, Vec2) {
        let Some(doc) = &self.doc else { return (Vec::new(), Vec2::ZERO) };
        let mut tops = Vec::with_capacity(doc.page_sizes.len());
        let mut y = MARGIN;
        let mut max_w: f32 = 0.0;
        for &page in &self.order {
            let size = doc.page_sizes[page];
            tops.push(y);
            y += size.y * self.zoom + GAP;
            max_w = max_w.max(size.x * self.zoom);
        }
        (tops, Vec2::new(max_w + 2.0 * MARGIN, y - GAP + MARGIN))
    }

    /// The spot being read, for the sidebar's sections: a position, and
    /// display points from the top of that page. It is a quarter of the way
    /// down the view, so a heading near the top counts as being read.
    fn reading_spot(&self) -> (usize, f32) {
        let (tops, _) = self.layout();
        let y = self.pending_offset.unwrap_or(self.last_offset).y + (self.viewport_size.y / 4.0).max(2.0 * MARGIN);
        let pos = tops.partition_point(|&t| t <= y).saturating_sub(1);
        let within = tops.get(pos).map_or(0.0, |&t| (y - t) / self.zoom);
        (pos, within.max(0.0))
    }

    fn go_to_page(&mut self, page: usize) {
        self.go_to(page, None);
    }

    /// Scrolls to `page`, optionally to the spot `top` (display points from
    /// the page's top edge), with a little context above it.
    fn go_to(&mut self, page: usize, top: Option<f32>) {
        let count = self.page_count();
        if count == 0 {
            return;
        }
        let page = page.min(count - 1);
        let (tops, _) = self.layout();
        let y = match top {
            Some(t) => tops[page] + t * self.zoom - 2.0 * MARGIN,
            None => tops[page] - MARGIN / 2.0,
        };
        self.pending_offset = Some(Vec2::new(self.last_offset.x, y.max(0.0)));
        self.current_page = page;
        self.page_input = (page + 1).to_string();
    }

    fn set_zoom(&mut self, zoom: f32) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() < 1e-4 {
            return;
        }
        // Keep the same spot of the document at the top of the view (or
        // the spot a pending jump is heading to).
        let ratio = zoom / self.zoom;
        let base = self.pending_offset.unwrap_or(self.last_offset);
        self.pending_offset = Some(base * ratio);
        self.zoom = zoom;
        self.zoomed_at = Some(Instant::now());
    }

    fn apply_fit(&mut self) {
        let Some(doc) = &self.doc else { return };
        let max_w = doc.page_sizes.iter().map(|s| s.x).fold(1.0, f32::max);
        let width_zoom = (self.viewport_size.x - 2.0 * MARGIN - 12.0) / max_w;
        let zoom = match self.fit {
            Fit::None => return,
            Fit::Width => width_zoom,
            Fit::Page => {
                let page = self.order.get(self.current_page).copied().unwrap_or(0);
                let size = doc.page_sizes.get(page).copied().unwrap_or(Vec2::splat(1.0));
                width_zoom.min((self.viewport_size.y - 2.0 * MARGIN) / size.y.max(1.0))
            }
        };
        let page = self.current_page;
        let changed = (zoom.clamp(MIN_ZOOM, MAX_ZOOM) - self.zoom).abs() > 1e-3;
        self.set_zoom(zoom);
        if changed && self.fit == Fit::Page {
            self.go_to_page(page);
        }
    }

    // ------------------------------------------------------------------
    // Keyboard
    // ------------------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        // Saving works while typing in a form field too.
        if self.doc.is_some() {
            if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::S) {
                self.save_as_dialog(After::Nothing);
            }
            if pressed(Modifiers::COMMAND, Key::S) && self.is_modified() {
                self.save_in_place(After::Nothing);
            }
        }
        if ctx.egui_wants_keyboard_input() {
            return; // typing in the page box or a form field
        }
        if pressed(Modifiers::COMMAND, Key::O) {
            self.open_dialog();
        }
        if self.doc.is_none() {
            return;
        }

        // (Shift first: plain F5 would also match Shift+F5.)
        if pressed(Modifiers::SHIFT, Key::F5) {
            self.start_presentation(ctx, true);
        }
        if pressed(Modifiers::NONE, Key::F5) {
            self.start_presentation(ctx, false);
        }
        if pressed(Modifiers::COMMAND, Key::P) && self.printing.is_none() {
            self.open_print_dialog(ctx);
        }
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::D) {
            self.set_dark_pages(!self.dark_pages);
        }
        // (Shift first: Ctrl+R would also match Ctrl+Shift+R.)
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::R) {
            self.rotate_pages(&[self.current_page], -90);
        }
        if pressed(Modifiers::COMMAND, Key::R) {
            self.rotate_pages(&[self.current_page], 90);
        }
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::H) {
            self.highlight_selection(self.comments.color, false);
        }
        if self.comments.placing && pressed(Modifiers::NONE, Key::Escape) {
            self.comments.placing = false;
        }
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::B) {
            self.show_sidebar = true;
            self.sidebar.toggle_view(&self.pool);
        }
        if pressed(Modifiers::COMMAND, Key::E) {
            self.set_edit_mode(!self.edit_mode);
        }

        // --- undo / redo (Ctrl+Shift+Z first: Ctrl+Z matches it too) ---
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || pressed(Modifiers::COMMAND, Key::Y) {
            self.redo();
        }
        if pressed(Modifiers::COMMAND, Key::Z) {
            self.undo();
        }

        // --- links ---
        let mouse_back = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra1));
        if pressed(Modifiers::ALT, Key::ArrowLeft) || mouse_back {
            self.go_back();
        }

        // --- text selection ---
        let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        if copy && let Some(text) = self.selected_text() {
            ctx.copy_text(text);
        }
        if pressed(Modifiers::COMMAND, Key::A) {
            self.select_all(self.order[self.current_page.min(self.order.len() - 1)]);
        }
        if pressed(Modifiers::NONE, Key::Escape) {
            self.selection = None;
        }
        // Shift + arrows / Home / End extend the selection.
        for (key, m) in [
            (Key::ArrowLeft, Move::Left),
            (Key::ArrowRight, Move::Right),
            (Key::ArrowUp, Move::Up),
            (Key::ArrowDown, Move::Down),
            (Key::Home, Move::LineStart),
            (Key::End, Move::LineEnd),
        ] {
            if pressed(Modifiers::SHIFT, key) {
                self.extend_selection(m);
            }
        }

        // --- zoom ---
        if pressed(Modifiers::COMMAND, Key::Plus) || pressed(Modifiers::COMMAND, Key::Equals) {
            self.fit = Fit::None;
            self.set_zoom(self.zoom * ZOOM_STEP);
        }
        if pressed(Modifiers::COMMAND, Key::Minus) {
            self.fit = Fit::None;
            self.set_zoom(self.zoom / ZOOM_STEP);
        }
        if pressed(Modifiers::COMMAND, Key::Num0) {
            self.fit = Fit::Width;
        }

        // --- navigation ---
        let page = self.current_page;
        if pressed(Modifiers::NONE, Key::PageDown) || pressed(Modifiers::NONE, Key::ArrowRight) {
            self.go_to_page(page + 1);
        }
        if pressed(Modifiers::NONE, Key::PageUp) || pressed(Modifiers::NONE, Key::ArrowLeft) {
            self.go_to_page(page.saturating_sub(1));
        }
        if pressed(Modifiers::NONE, Key::Home) {
            self.go_to_page(0);
        }
        if pressed(Modifiers::NONE, Key::End) {
            self.go_to_page(usize::MAX);
        }
        if pressed(Modifiers::NONE, Key::ArrowDown) {
            self.pending_offset = Some(self.last_offset + Vec2::new(0.0, WHEEL_SCROLL));
        }
        if pressed(Modifiers::NONE, Key::ArrowUp) {
            self.pending_offset = Some((self.last_offset - Vec2::new(0.0, WHEEL_SCROLL)).max(Vec2::ZERO));
        }
        if pressed(Modifiers::NONE, Key::Space) {
            self.pending_offset = Some(self.last_offset + Vec2::new(0.0, self.viewport_size.y * 0.9));
        }
    }

    fn selected_text(&self) -> Option<String> {
        let sel = self.selection.filter(|s| !s.is_empty())?;
        let chars = self.chars.get(&sel.page)?;
        Some(selection::selected_text(chars, sel.range())).filter(|t| !t.is_empty())
    }

    fn select_all(&mut self, page: usize) {
        match self.chars.get(&page) {
            Some(chars) => self.selection = Some(Selection { page, anchor: 0, focus: chars.len() }),
            None => {
                self.select_all_pending = Some(page);
                self.request_chars(page);
            }
        }
    }

    fn extend_selection(&mut self, m: Move) {
        let page = self.selection.map_or_else(|| self.order.get(self.current_page).copied().unwrap_or(0), |s| s.page);
        let Some(chars) = self.chars.get(&page) else {
            self.request_chars(page);
            return;
        };
        let mut sel = self.selection.unwrap_or(Selection { page, anchor: 0, focus: 0 });
        sel.focus = selection::move_caret(chars, sel.focus, m);
        self.selection = Some(sel);
    }

    /// Follows a link: web addresses open in the browser, links inside the
    /// document scroll there (Alt+← comes back).
    fn follow_link(&mut self, ctx: &egui::Context, target: &LinkTarget) {
        match target {
            LinkTarget::Uri(uri) => {
                if let Some(url) = links::openable_url(uri) {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
            LinkTarget::Page { page, top } => {
                self.back.push(self.pending_offset.unwrap_or(self.last_offset));
                if self.back.len() > 100 {
                    self.back.remove(0);
                }
                self.go_to(self.position_of(*page), *top);
            }
        }
    }

    /// Returns to where the last followed link was clicked.
    fn go_back(&mut self) {
        if let Some(offset) = self.back.pop() {
            self.pending_offset = Some(offset);
        }
    }

    fn request_chars(&mut self, page: usize) {
        if !self.chars.contains_key(&page) && self.chars_requested.insert(page) {
            self.pool.want_chars(page);
        }
    }

    // ------------------------------------------------------------------
    // Panels
    // ------------------------------------------------------------------

    /// The toolbar, which is also the window's title bar. The window
    /// buttons always keep their place at the right; when the window is too
    /// narrow for everything, less used controls move into a "…" menu, and
    /// then the main buttons drop their labels.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let bar = ui.max_rect();
        titlebar::drag_area(ui, bar);
        // The fullest layout that fit when last measured.
        let more = if self.doc.is_some() { 38.0 } else { 0.0 };
        let split = bar.right() - titlebar::BUTTONS_WIDTH - more;
        let controls = Rect::from_min_max(bar.min, Pos2::new(split - 8.0, bar.bottom()));
        let available = controls.width();
        let level = (0..2).find(|&l| self.toolbar_widths[l].is_none_or(|w| w <= available)).unwrap_or(2);

        let buttons = Rect::from_min_max(Pos2::new(split, bar.top()), bar.max);
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(buttons).layout(egui::Layout::right_to_left(egui::Align::Center)),
            |ui| {
                ui.add_space(4.0);
                titlebar::window_buttons(ui);
                if self.doc.is_some() {
                    ui.add_space(4.0);
                    self.more_menu(ui, level);
                }
            },
        );

        let mut used = 0.0;
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(controls).layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(controls.intersect(ui.clip_rect()));
                ui.add_space(8.0);
                ui.spacing_mut().item_spacing.x = 4.0;
                self.toolbar_controls(ui, level);
                used = ui.cursor().min.x - controls.left();
                // The file name, centered in whatever room is left.
                let free = Rect::from_min_max(Pos2::new(ui.cursor().min.x + 12.0, bar.top()), controls.max);
                if let Some(doc) = &self.doc
                    && free.width() > 60.0
                {
                    let p = theme::palette(ui);
                    let name = doc.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let mut job = egui::text::LayoutJob::default();
                    if self.is_modified() {
                        // Unsaved changes.
                        job.append("● ", 0.0, egui::TextFormat::simple(egui::FontId::proportional(10.0), p.accent));
                    }
                    job.append(&name, 0.0, egui::TextFormat::simple(egui::FontId::proportional(14.0), p.text));
                    job.wrap = egui::text::TextWrapping::truncate_at_width(free.width());
                    job.halign = egui::Align::Center;
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(free)
                            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight)),
                        |ui| ui.add(egui::Label::new(job).selectable(false)),
                    );
                }
            },
        );
        if level < 2 {
            self.toolbar_widths[level] = Some(used);
        }
    }

    /// The toolbar's buttons. `level` 0 shows labels with the main buttons;
    /// 1 shows icons only; 2 also moves undo and redo into the menu.
    fn toolbar_controls(&mut self, ui: &mut egui::Ui, level: usize) {
        let labels = level == 0;
        if icons::button(ui, Icon::Open, labels.then_some("Open"), false, true)
            .on_hover_text("Open a PDF (Ctrl+O)")
            .clicked()
        {
            self.open_dialog();
        }
        if self.doc.is_none() {
            return;
        }
        let can_save = self.is_modified() && self.saving.is_none();
        if icons::button(ui, Icon::Save, labels.then_some("Save"), false, can_save)
            .on_hover_text("Save the changes (Ctrl+S)")
            .clicked()
        {
            self.save_in_place(After::Nothing);
        }
        if level < 2 {
            divider(ui);
            if icons::button(ui, Icon::Undo, None, false, !self.undo.is_empty()).on_hover_text("Undo (Ctrl+Z)").clicked() {
                self.undo();
            }
            if icons::button(ui, Icon::Redo, None, false, !self.redo.is_empty())
                .on_hover_text("Redo (Ctrl+Shift+Z or Ctrl+Y)")
                .clicked()
            {
                self.redo();
            }
        }
        divider(ui);
        icons::group(ui, |ui| {
            if icons::button(ui, Icon::Edit, labels.then_some("Edit text"), self.edit_mode, true)
                .on_hover_text("Click text to change it, or an empty spot to add text (Ctrl+E)")
                .clicked()
            {
                self.set_edit_mode(!self.edit_mode);
            }
            if icons::button(ui, Icon::Crop, labels.then_some("Crop"), self.crop_mode, true)
                .on_hover_text("Crop pages to an area you choose")
                .clicked()
            {
                self.set_crop_mode(!self.crop_mode);
            }
            if icons::button(ui, Icon::RotateRight, labels.then_some("Rotate"), self.rotate_mode, true)
                .on_hover_text("Rotate this page, some pages or all of them (Ctrl+R / Ctrl+Shift+R: this page)")
                .clicked()
            {
                self.set_rotate_mode(!self.rotate_mode);
            }
            if icons::button(ui, Icon::Redact, labels.then_some("Redact"), self.redact_mode, true)
                .on_hover_text("Black out text, images or areas, removing what was there for good")
                .clicked()
            {
                self.set_redact_mode(!self.redact_mode);
            }
            if icons::button(ui, Icon::Comment, labels.then_some("Comment"), self.comments.placing, true)
                .on_hover_text("Add a comment anywhere: click here, then on the spot. To highlight or comment on text, select it.")
                .clicked()
            {
                self.set_placing_note(!self.comments.placing);
            }
        });
        divider(ui);
        if icons::button(ui, Icon::Sidebar, None, self.show_sidebar, true)
            .on_hover_text("Show or hide the page thumbnails")
            .clicked()
        {
            self.show_sidebar = !self.show_sidebar;
        }
        if icons::button(ui, Icon::Present, None, false, true)
            .on_hover_text("Present: pages as full-screen slides (F5 from the start, Shift+F5 from this page)")
            .clicked()
        {
            self.start_presentation(ui.ctx(), false);
        }
        if icons::button(ui, Icon::Moon, None, self.dark_pages, true)
            .on_hover_text("Dark pages for reading: light text on a dark page, colors kept (Ctrl+Shift+D)")
            .clicked()
        {
            self.set_dark_pages(!self.dark_pages);
        }
    }

    /// The "⋯" menu: saving a copy, compressing, document info, appearance
    /// (and undo/redo when the toolbar is narrow).
    fn more_menu(&mut self, ui: &mut egui::Ui, level: usize) {
        let response = icons::button(ui, Icon::More, None, false, true).on_hover_text("More");
        // The menu stays open while choosing the appearance; the other
        // items close it.
        egui::Popup::menu(&response).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(200.0);
            if level >= 2 {
                if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo").shortcut_text("Ctrl+Z")).clicked() {
                    self.undo();
                    ui.close();
                }
                if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo").shortcut_text("Ctrl+Y")).clicked() {
                    self.redo();
                    ui.close();
                }
                ui.separator();
            }
            if ui
                .add_enabled(self.saving.is_none(), egui::Button::new("Save as…").shortcut_text("Ctrl+Shift+S"))
                .clicked()
            {
                ui.close();
                self.save_as_dialog(After::Nothing);
            }
            if ui.button("Delete pages…").on_hover_text("Delete a page, or a list or range of pages").clicked() {
                ui.close();
                self.delete_dialog = Some(((self.current_page + 1).to_string(), None));
            }
            ui.separator();
            if ui.add(egui::Button::new("Present").shortcut_text("F5")).clicked() {
                ui.close();
                self.start_presentation(ui.ctx(), false);
            }
            if ui.add(egui::Button::new("Present from this page").shortcut_text("Shift+F5")).clicked() {
                ui.close();
                self.start_presentation(ui.ctx(), true);
            }
            ui.checkbox(&mut self.presenter_view, "Presenter view when presenting")
                .on_hover_text("A second window with the next slide, notes and a timer (P switches it while presenting)");
            ui.separator();
            if ui
                .add_enabled(self.printing.is_none(), egui::Button::new("Print…").shortcut_text("Ctrl+P"))
                .clicked()
            {
                ui.close();
                self.open_print_dialog(ui.ctx());
            }
            if ui
                .add_enabled(self.saving.is_none() && self.compressing.is_none(), egui::Button::new("Compress…"))
                .on_hover_text("Save a smaller copy that loses nothing")
                .clicked()
            {
                ui.close();
                self.compress.open();
            }
            ui.separator();
            if ui.selectable_label(self.show_info, "Document info").clicked() {
                self.show_info = !self.show_info;
                ui.close();
            }
            ui.separator();
            ui.weak("Appearance");
            let mut dark = self.dark_pages;
            if ui.checkbox(&mut dark, "Dark pages for reading").on_hover_text("Ctrl+Shift+D").changed() {
                self.set_dark_pages(dark);
            }
            let before = self.theme;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.theme, egui::ThemePreference::System, "System");
                ui.selectable_value(&mut self.theme, egui::ThemePreference::Light, "Light");
                ui.selectable_value(&mut self.theme, egui::ThemePreference::Dark, "Dark");
            });
            if self.theme != before {
                ui.ctx().set_theme(self.theme);
            }
        });
    }

    /// Page navigation and zoom, floating at the bottom of the page view.
    fn page_pill(&mut self, ctx: &egui::Context) {
        let view = self.view_rect;
        if !view.is_positive() || view.width() < 260.0 {
            return;
        }
        egui::Area::new(egui::Id::new("page-pill"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.center().x, view.bottom() - 14.0))
            .pivot(egui::Align2::CENTER_BOTTOM)
            .show(ctx, |ui| {
                theme::floating_frame(ui).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        self.page_controls(ui);
                        divider(ui);
                        self.zoom_controls(ui);
                    });
                });
            });
    }
    fn page_controls(&mut self, ui: &mut egui::Ui) {
        let count = self.page_count();
        if icons::button(ui, Icon::Left, None, false, self.current_page > 0).on_hover_text("Previous page").clicked() {
            self.go_to_page(self.current_page - 1);
        }
        let input = ui.add(
            egui::TextEdit::singleline(&mut self.page_input)
                .desired_width(34.0)
                .horizontal_align(egui::Align::Center)
                .margin(egui::Margin::symmetric(4, 4)),
        );
        if input.lost_focus() {
            match self.page_input.trim().parse::<usize>() {
                Ok(n) if n >= 1 => self.go_to_page(n - 1),
                _ => self.page_input = (self.current_page + 1).to_string(),
            }
        }
        ui.label(egui::RichText::new(format!("/ {count}")).color(theme::palette(ui).weak));
        if icons::button(ui, Icon::Right, None, false, self.current_page + 1 < count).on_hover_text("Next page").clicked() {
            self.go_to_page(self.current_page + 1);
        }
    }

    fn zoom_controls(&mut self, ui: &mut egui::Ui) {
        if icons::button(ui, Icon::Minus, None, false, true).on_hover_text("Zoom out (Ctrl+-)").clicked() {
            self.fit = Fit::None;
            self.set_zoom(self.zoom / ZOOM_STEP);
        }
        let label = format!("{:.0}%", self.zoom * 100.0);
        let zoom = ui
            .add(egui::Button::new(label).frame(false).min_size(Vec2::new(52.0, 30.0)))
            .on_hover_text("Zoom options");
        egui::Popup::menu(&zoom).show(|ui| {
            ui.set_min_width(150.0);
            if ui.selectable_label(self.fit == Fit::Width, "Fit width").clicked() {
                self.fit = Fit::Width;
            }
            if ui.selectable_label(self.fit == Fit::Page, "Fit page").clicked() {
                self.fit = Fit::Page;
            }
            ui.separator();
            for z in [0.5, 0.75, 1.0, 1.5, 2.0, 3.0] {
                let current = self.fit == Fit::None && (self.zoom - z).abs() < 1e-3;
                if ui.selectable_label(current, format!("{:.0}%", z * 100.0)).clicked() {
                    self.fit = Fit::None;
                    self.set_zoom(z);
                }
            }
        });
        if icons::button(ui, Icon::Plus, None, false, true).on_hover_text("Zoom in (Ctrl++)").clicked() {
            self.fit = Fit::None;
            self.set_zoom(self.zoom * ZOOM_STEP);
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.style_mut().override_text_style = Some(egui::TextStyle::Small);
        ui.visuals_mut().override_text_color = Some(p.weak);
        ui.horizontal_centered(|ui| {
            if let Some(err) = &self.error {
                ui.colored_label(p.danger, err);
                return;
            }
            if let Some(path) = &self.loading {
                ui.spinner();
                ui.label(format!("Opening {}…", path.display()));
                return;
            }
            let Some(doc) = &self.doc else {
                ui.label("No document open");
                return;
            };
            // Details on the right.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("PDF {} · {} pages", doc.version, self.order.len()));
                if doc.repaired {
                    ui.colored_label(p.warning, "damaged file (repaired)");
                }
                if doc.encrypted {
                    ui.colored_label(p.warning, "encrypted — not supported yet");
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| self.status_message(ui));
            });
        });
    }

    /// What is going on: background work, the current mode, unsaved
    /// changes, the selection.
    fn status_message(&self, ui: &mut egui::Ui) {
        if let Some((path, _)) = self.inserting.first() {
            ui.spinner();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ui.label(format!("Adding {name}…"));
        } else if let Some(saving) = &self.saving {
            ui.spinner();
            ui.label(format!("Saving {}…", saving.target.display()));
        } else if self.crop_mode {
            ui.label("Crop — drag on a page to choose the area to keep");
        } else if self.redact_mode {
            ui.label("Redact — mark what to hide, then click Redact");
        } else if self.edit_mode {
            ui.label(if self.editor.is_editing() {
                "Editing — click outside the text to apply, Esc to cancel"
            } else {
                "Edit text — click text to edit it, or an empty spot to add text"
            });
        } else if let Some(text) = self.selected_text() {
            ui.label(format!("{} characters selected — Ctrl+C to copy", text.chars().count()));
        } else if self.is_modified() {
            ui.label(if self.pages_modified() || self.editor.is_modified() {
                "Unsaved changes — Ctrl+S to save, Ctrl+Z to undo"
            } else {
                "Form filled in — Ctrl+S to save"
            });
        }
    }

    /// Document information, in a separate window that can be moved
    /// outside the viewer and resized from its edges and corners.
    fn info_window(&mut self, ctx: &egui::Context) {
        if !self.show_info {
            return;
        }
        let Some(doc) = &self.doc else { return };
        let mut rows: Vec<(String, String)> = vec![
            ("File".into(), doc.path.display().to_string()),
            ("File size".into(), file_size(doc.file_size)),
            ("PDF version".into(), doc.version.clone()),
            ("Pages".into(), self.order.len().to_string()),
        ];
        if let Some(size) = self.order.get(self.current_page).and_then(|&p| doc.page_sizes.get(p)) {
            rows.push((
                "Page size".into(),
                format!(
                    "{:.0} × {:.0} pt ({:.0} × {:.0} mm)",
                    size.x,
                    size.y,
                    size.x * 25.4 / 72.0,
                    size.y * 25.4 / 72.0
                ),
            ));
        }
        rows.extend(doc.metadata.iter().map(|(k, v)| (k.to_string(), v.clone())));

        let builder = egui::ViewportBuilder::default()
            .with_title("Document info")
            .with_inner_size([480.0, 360.0])
            .with_min_inner_size([260.0, 160.0])
            .with_decorations(titlebar::NATIVE); // same title bar as the viewer
        let mut open = true;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("document-info"), builder, |ui, _class| {
            let ctx = ui.ctx().clone();
            if ctx.input(|i| i.viewport().close_requested() || i.key_pressed(Key::Escape)) {
                open = false;
            }
            titlebar::resize_edges(&ctx);
        self.update_title(&ctx);
            egui::Panel::top("info-title").exact_size(32.0).show(ui, |ui| {
                titlebar::drag_area(ui, ui.max_rect());
                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    ui.strong("Document info");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), titlebar::close_button);
                });
            });
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| info_rows(ui, &rows));
            });
        });
        self.show_info = open;
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() / 2.0 - 140.0).max(24.0));
            let (r, _) = ui.allocate_exact_size(Vec2::splat(88.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 44.0, p.accent_soft);
            icons::paint(ui.painter(), r.shrink(22.0), Icon::Document, p.accent);
            ui.add_space(20.0);
            ui.label(egui::RichText::new("Open a PDF").size(24.0).color(p.text));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Drop a file here, or press Ctrl+O").color(p.weak));
            ui.add_space(20.0);
            let open = egui::Button::new(egui::RichText::new("Choose a file…").size(15.0).color(p.on_accent))
                .fill(p.accent)
                .corner_radius(9)
                .min_size(Vec2::new(160.0, 38.0));
            if ui.add(open).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.open_dialog();
            }
        });
    }

    // ------------------------------------------------------------------
    // Pages
    // ------------------------------------------------------------------

    fn pages(&mut self, ui: &mut egui::Ui) -> CommentAnchors {
        let mut anchors = CommentAnchors::default();
        // Ctrl + mouse wheel zooms.
        let zoom_delta = ui.input(|i| i.zoom_delta());
        if zoom_delta != 1.0 && ui.ui_contains_pointer() {
            self.fit = Fit::None;
            self.set_zoom(self.zoom * zoom_delta);
        }
        self.viewport_size = ui.available_size();
        self.apply_fit();

        let (tops, content) = self.layout();
        let Some(doc) = &self.doc else { return anchors };
        let sizes: Vec<Vec2> = self.order.iter().map(|&p| doc.page_sizes[p] * self.zoom).collect();

        // Mouse drags select text; egui only drag-scrolls on touch screens.
        let mut area = egui::ScrollArea::both().auto_shrink(false);
        if let Some(offset) = self.pending_offset.take() {
            area = area.scroll_offset(offset);
        }
        let ppp = ui.ctx().pixels_per_point();
        let zoom = self.zoom;
        // Visible pages (by page, with their screen rectangles) and their positions.
        let mut visible: Vec<(usize, Rect)> = Vec::new();
        let mut visible_positions: Vec<usize> = Vec::new();
        let order = self.order.clone();
        let mut current = self.current_page;
        let mut screen_viewport = Rect::NOTHING;

        let output = area.show_viewport(ui, |ui, viewport| {
            let width = content.x.max(viewport.width());
            ui.set_min_size(Vec2::new(width, content.y));
            let origin = ui.max_rect().min;
            screen_viewport = viewport.translate(origin.to_vec2());
            let painter = ui.painter().clone();
            let page_shadow = theme::palette(ui).shadow;

            // The last page starting above the viewport may still overlap it.
            let first = tops.partition_point(|&t| t < viewport.min.y).saturating_sub(1);
            for i in first..tops.len() {
                let top = tops[i];
                if top > viewport.max.y {
                    break;
                }
                let size = sizes[i];
                if top + size.y < viewport.min.y {
                    continue;
                }
                let rect = Rect::from_min_size(origin + Vec2::new((width - size.x) / 2.0, top), size);
                let page = order[i];
                visible.push((page, rect));
                visible_positions.push(i);
                painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 14, spread: 0, color: page_shadow }.as_shape(rect, 1));
                painter.rect_filled(rect, 0.0, theme::paper(self.dark_pages));
                if let Some(t) = self.textures.get_mut(&page) {
                    t.last_used = self.frame;
                    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                    painter.image(t.texture.id(), crisp_rect(rect, t, zoom * ppp, ppp), uv, Color32::WHITE);
                } else {
                    let (msg, color) = match self.failed.get(&page) {
                        Some(err) => (format!("Could not render page {}\n{err}", i + 1), Color32::from_rgb(180, 60, 60)),
                        None => (String::new(), Color32::GRAY),
                    };
                    painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(14.0), color);
                }
                // Selection highlight.
                if let (Some(sel), Some(chars)) = (self.selection, self.chars.get(&page))
                    && sel.page == page
                {
                    for r in selection::highlight_rects(chars, sel.range()) {
                        let r = Rect::from_min_max(rect.min + r.min.to_vec2() * zoom, rect.min + r.max.to_vec2() * zoom);
                        painter.rect_filled(r.expand(0.5), 1.0, SELECTION_COLOR);
                        if !self.dragging && anchors.selection.is_none() {
                            anchors.selection = Some(r);
                        }
                    }
                }
                // The comment whose box is open: outlined.
                if let Some(open) = &self.comments.open
                    && self.sources.get(page) == Some(&(0, open.page))
                    && let Some(c) = self.comments.find(&self.editor, open.page, open.id)
                {
                    let accent = theme::palette(ui).accent;
                    let mut all = Rect::NOTHING;
                    for &[l, t, r, b] in &c.areas {
                        let r = Rect::from_min_max(rect.min + Vec2::new(l, t) * zoom, rect.min + Vec2::new(r, b) * zoom);
                        painter.rect_stroke(r.expand(2.0), 3.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Outside);
                        all = all.union(r);
                    }
                    anchors.open = Some(all);
                }
            }
            // The current page is the one under the upper third of the view.
            let probe = viewport.min.y + viewport.height() / 3.0;
            current = tops.partition_point(|&t| t <= probe).saturating_sub(1);

            // Mouse interaction over the visible area: text selection, or
            // in edit mode, editing.
            if self.crop_mode {
                for (&(key, rect), &position) in visible.iter().zip(&visible_positions) {
                    if let Some(&(0, page)) = self.sources.get(key) {
                        self.crop.show_page(ui, page, position, rect, zoom);
                    }
                }
            } else if self.redact_mode {
                for (&(key, rect), &position) in visible.iter().zip(&visible_positions) {
                    if let Some(&(0, page)) = self.sources.get(key) {
                        self.redact.show_page(ui, page, position, rect, zoom);
                    }
                }
            } else if self.edit_mode {
                for &(key, rect) in &visible {
                    if let Some(&(0, page)) = self.sources.get(key)
                        && let Some(event) = self.editor.show_page(ui, page, rect, zoom)
                    {
                        self.handle_editor_event(event);
                    }
                }
            } else if self.comments.placing {
                // The Comment tool: click where the note goes.
                let response = ui.interact(screen_viewport, ui.id().with("place-note"), Sense::click());
                let at = response.hover_pos().and_then(|p| visible.iter().find(|(_, r)| r.contains(p)).map(|&(k, r)| (k, r, p)));
                if let Some((key, rect, p)) = at
                    && let Some(&(0, page)) = self.sources.get(key)
                {
                    ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                    let response = response.on_hover_text_at_pointer("Click to place a comment here (Esc: cancel)");
                    if response.clicked() {
                        self.place_note(page, ((p - rect.min) / zoom).to_pos2());
                    }
                }
            } else {
                let response = ui.interact(screen_viewport, ui.id().with("pages"), Sense::click_and_drag());
                self.pointer_selection(ui, &response, &visible);
                // Comments under the pointer: a click opens them.
                let hit = response.hover_pos().and_then(|p| {
                    let &(key, rect) = visible.iter().find(|(_, r)| r.contains(p))?;
                    let &(0, page) = self.sources.get(key)? else { return None };
                    self.comments.at(&self.editor, page, ((p - rect.min) / zoom).to_pos2())
                });
                if let Some(c) = hit
                    && !self.dragging
                {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    if self.comments.open.as_ref().is_none_or(|o| o.id != c.id) {
                        let tip = if c.text.trim().is_empty() {
                            format!("{} — click to add a comment", comments::kind_name(&c.kind))
                        } else {
                            c.text.clone()
                        };
                        response.clone().on_hover_text_at_pointer(tip);
                    }
                    if response.clicked() && self.selection.is_none_or(|s| s.is_empty()) {
                        self.open_comment(&c, false);
                    }
                }
            }

            // Form fields, on top of the page and the selection area.
            for &(key, rect) in &visible {
                if let Some(&(0, page)) = self.sources.get(key) {
                    self.form.show_page(ui, page, rect, zoom);
                }
            }
        });

        self.view_rect = screen_viewport;
        if output.state.offset.y != self.last_offset.y {
            self.scrolling_down = output.state.offset.y > self.last_offset.y;
        }
        self.last_offset = output.state.offset;
        if current != self.current_page {
            self.current_page = current;
            if !ui.ctx().egui_wants_keyboard_input() {
                self.page_input = (current + 1).to_string();
            }
        }
        // While selecting, scroll when the pointer goes past the top/bottom.
        if self.dragging
            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        {
            let dy = if p.y < screen_viewport.top() {
                p.y - screen_viewport.top()
            } else if p.y > screen_viewport.bottom() {
                p.y - screen_viewport.bottom()
            } else {
                0.0
            };
            if dy != 0.0 {
                self.pending_offset = Some(self.last_offset + Vec2::new(0.0, dy * 0.5));
                ui.ctx().request_repaint_after(FRAME);
            }
        }
        self.request_pages(ui.ctx(), &visible_positions, ppp);
        anchors
    }

    /// Mouse text selection: press and drag to select, double-click for a
    /// word, triple-click for a line, Shift+click to extend.
    fn pointer_selection(&mut self, ui: &egui::Ui, response: &egui::Response, visible: &[(usize, Rect)]) {
        let zoom = self.zoom;
        let to_page = |rect: Rect, p: Pos2| ((p - rect.min) / zoom).to_pos2();
        let page_at = |p: Pos2| visible.iter().find(|(_, r)| r.contains(p)).copied();

        // Links: a double-click follows a link within the document;
        // web links open only with Ctrl+click (Ctrl+click works for both).
        // A plain click or drag still selects text.
        let ctrl = ui.input(|i| i.modifiers.command);
        let hovered_link = response
            .hover_pos()
            .and_then(|p| page_at(p).map(|(i, rect)| (i, to_page(rect, p))))
            .and_then(|(i, p)| links::link_at(self.links.get(&i)?, p).cloned());
        let internal = hovered_link.as_ref().is_some_and(|l| matches!(l.target, LinkTarget::Page { .. }));
        if let Some(link) = &hovered_link {
            let double_clicked = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
            if internal && double_clicked && !ctrl {
                // The double-click's first click started a selection; drop it.
                self.selection = None;
                self.dragging = false;
                self.follow_link(ui.ctx(), &link.target);
                return;
            }
            if !self.dragging {
                // Page numbers as currently arranged.
                let shown = match link.target {
                    LinkTarget::Page { page, top } => LinkTarget::Page { page: self.position_of(page), top },
                    ref other => other.clone(),
                };
                response.clone().on_hover_text_at_pointer(links::tooltip(&shown));
                if ctrl {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    if ui.input(|i| i.pointer.primary_pressed()) {
                        self.follow_link(ui.ctx(), &link.target);
                    }
                    return;
                }
            }
        }

        // Pointing hand over links, text cursor over other text.
        if let Some(p) = response.hover_pos()
            && let Some((i, rect)) = page_at(p)
        {
            match self.chars.get(&i) {
                _ if hovered_link.is_some() && !self.dragging => ui.ctx().set_cursor_icon(CursorIcon::PointingHand),
                Some(chars) if selection::is_over_text(chars, to_page(rect, p)) => {
                    ui.ctx().set_cursor_icon(CursorIcon::Text);
                }
                Some(_) => {}
                None => self.request_chars(i),
            }
        }

        let (pressed, shift, clicks) = ui.input(|i| {
            let clicks = if i.pointer.button_triple_clicked(egui::PointerButton::Primary) {
                3
            } else if i.pointer.button_double_clicked(egui::PointerButton::Primary) {
                2
            } else {
                0
            };
            (i.pointer.primary_pressed(), i.modifiers.shift, clicks)
        });
        let Some(p) = response.interact_pointer_pos().or(response.hover_pos()) else { return };

        if pressed && response.contains_pointer() {
            match page_at(p) {
                Some((i, rect)) => match self.chars.get(&i) {
                    Some(chars) => {
                        let caret = selection::caret_at(chars, to_page(rect, p)).unwrap_or(0);
                        match self.selection {
                            Some(mut sel) if shift && sel.page == i => {
                                sel.focus = caret;
                                self.selection = Some(sel);
                            }
                            _ => self.selection = Some(Selection { page: i, anchor: caret, focus: caret }),
                        }
                        self.dragging = true;
                    }
                    None => {
                        self.selection = None;
                        self.request_chars(i);
                    }
                },
                None => self.selection = None,
            }
        }
        if clicks >= 2
            && let Some((i, rect)) = page_at(p)
            && let Some(chars) = self.chars.get(&i)
            && let Some(caret) = selection::caret_at(chars, to_page(rect, p))
        {
            let at = caret.min(chars.len().saturating_sub(1));
            let range = if clicks == 3 { selection::line_at(chars, at) } else { selection::word_at(chars, at) };
            self.selection = Some(Selection { page: i, anchor: range.start, focus: range.end });
            self.dragging = false;
        }

        if self.dragging {
            if !ui.input(|i| i.pointer.primary_down()) {
                self.dragging = false;
            } else if let Some(mut sel) = self.selection
                && let Some((_, rect)) = visible.iter().find(|(i, _)| *i == sel.page)
                && let Some(chars) = self.chars.get(&sel.page)
            {
                // Measured against the selection's page even when the pointer
                // leaves it, so dragging past the edge selects to the end.
                if let Some(caret) = selection::caret_at(chars, to_page(*rect, p)) {
                    sel.focus = caret;
                    self.selection = Some(sel);
                }
            }
        }
    }

    /// Tells the workers which pages to render, prefetching ahead in the
    /// scroll direction, and frees textures beyond the memory budget.
    /// `visible` are positions.
    fn request_pages(&mut self, ctx: &egui::Context, visible: &[usize], ppp: f32) {
        let count = self.page_count();
        if count == 0 || visible.is_empty() {
            return;
        }
        // Close to exact so the bitmap maps 1:1 onto screen pixels (see crisp_rect).
        let scale = ((self.zoom * ppp) * 1000.0).round() / 1000.0;
        let (first, last) = (visible[0], visible[visible.len() - 1]);

        // Visible pages first (middle of the view outward), then neighbors.
        let mid = (first + last) / 2;
        let mut order: Vec<usize> = visible.to_vec();
        order.sort_by_key(|&p| p.abs_diff(mid));
        let down = self.scrolling_down;
        let ahead = |k: usize| if down { last.checked_add(k) } else { first.checked_sub(k) };
        let behind = |k: usize| if down { first.checked_sub(k) } else { last.checked_add(k) };
        order.extend([ahead(1), ahead(2), behind(1)].into_iter().flatten().filter(|&p| p < count));
        // From positions to pages.
        let order: Vec<usize> = order.iter().map(|&pos| self.order[pos]).collect();

        // Text for visible pages, for selection.
        for &pos in visible {
            self.request_chars(self.order[pos]);
        }

        // While a zoom is settling, keep showing the existing (stretched)
        // bitmaps instead of re-rendering at every intermediate size.
        let settling = self.zoomed_at.is_some_and(|t| t.elapsed() < ZOOM_SETTLE);
        if settling {
            ctx.request_repaint_after(ZOOM_SETTLE);
        }
        let want: Vec<usize> = order
            .iter()
            .copied()
            .filter(|p| !self.failed.contains_key(p))
            .filter(|p| match self.textures.get(p) {
                None => true,
                Some(t) => !settling && (t.scale / scale - 1.0).abs() > SCALE_TOLERANCE,
            })
            .collect();
        if (want.clone(), scale) != self.last_want {
            self.last_want = (want.clone(), scale);
            self.pool.want_pages(&want, scale);
        }

        // Evict the least recently shown textures beyond the budget.
        let mut total: usize = self.textures.values().map(|t| t.bytes).sum();
        if total > TEXTURE_BUDGET {
            let mut lru: Vec<(u64, usize)> =
                self.textures.iter().filter(|(p, _)| !order.contains(p)).map(|(p, t)| (t.last_used, *p)).collect();
            lru.sort();
            for (_, p) in lru {
                if total <= TEXTURE_BUDGET {
                    break;
                }
                if let Some(t) = self.textures.remove(&p) {
                    total -= t.bytes;
                }
            }
        }
        // Text of pages far from the view isn't needed.
        let selected = self.selection.map(|s| s.page);
        let keep_text = |p: &usize| order.contains(p) || selected == Some(*p);
        self.chars.retain(|p, _| keep_text(p));
        self.chars_requested.retain(keep_text);
    }
}

impl eframe::App for Viewer {
    /// `--script`: the next scripted input.
    #[cfg(feature = "script")]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.screenshot.is_some() || self.loading.is_some() || (self.doc.is_none() && self.error.is_none()) {
            ctx.request_repaint();
            return; // waiting for a screenshot, or for the document
        }
        let Some(script) = &mut self.script else { return };
        match script.feed(raw) {
            script::Next::Continue => {}
            script::Next::Shot(path) => {
                self.screenshot = Some(Screenshot { path, requested: false, settle: 3, exit: false });
            }
            script::Next::Done => {
                self.script = None;
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        ctx.request_repaint();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame += 1;
        self.handle_replies(&ctx);
        self.poll_save(&ctx);
        // Closing with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && self.is_modified() && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.prompt.is_none() {
                self.prompt = Some(After::Close);
            }
        }
        if self.present.is_some() && self.doc.is_some() {
            self.presentation(ui, frame);
            self.take_screenshot(&ctx);
            return;
        }
        titlebar::resize_edges(&ctx);

        // Files dropped onto the window.
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone())) {
            self.open(path);
        }
        self.handle_keys(&ctx);

        let palette = theme::of(&ctx.global_style().visuals);
        egui::Panel::top("toolbar")
            .exact_size(46.0)
            .frame(egui::Frame::new().fill(palette.panel))
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status")
            .exact_size(26.0)
            .frame(egui::Frame::new().fill(palette.panel).inner_margin(egui::Margin::symmetric(12, 0)))
            .show(ui, |ui| self.status_bar(ui));
        if self.show_sidebar
            && let Some(doc) = &self.doc
        {
            let sizes = doc.page_sizes.clone();
            let reading = self.reading_spot();
            let rows = if self.sidebar.view() == sidebar::View::Comments { self.comment_rows() } else { Vec::new() };
            let open_comment = self.comments.open.as_ref().map(|o| o.id);
            let mut action = None;
            // Each view keeps its own width: sections need room for titles.
            let (id, width) = match self.sidebar.view() {
                sidebar::View::Pages => ("sidebar", sidebar::DEFAULT_WIDTH),
                sidebar::View::Sections => ("sidebar-sections", sidebar::SECTIONS_WIDTH),
                sidebar::View::Comments => ("sidebar-comments", sidebar::SECTIONS_WIDTH),
            };
            egui::Panel::left(id)
                .resizable(true)
                .default_size(width)
                .size_range(100.0..=400.0)
                .show(ui, |ui| {
                    action = self.sidebar.show(
                        ui,
                        &sizes,
                        &self.order,
                        reading,
                        sidebar::CommentList { rows: &rows, open: open_comment },
                        &self.pool,
                    );
                });
            match action {
                Some(sidebar::Action::GoTo(pos)) => self.go_to_page(pos),
                Some(sidebar::Action::Section(LinkTarget::Page { page, top })) => {
                    // (Not to a deleted page.)
                    if self.order.contains(&page) {
                        self.go_to(self.position_of(page), top)
                    }
                }
                Some(sidebar::Action::Section(target)) => self.follow_link(ui.ctx(), &target),
                Some(sidebar::Action::Comment { id, page }) => {
                    if let Some(c) = self.comments.find(&self.editor, page, id) {
                        self.go_to(self.position_of(page), Some(c.rect[1]));
                        self.open_comment(&c, false);
                    }
                }
                Some(sidebar::Action::Move { from, to }) => self.move_page(from, to),
                Some(sidebar::Action::Insert(pos)) => self.insert_file_dialog(pos),
                Some(sidebar::Action::Delete(positions)) => self.delete_pages(&positions),
                Some(sidebar::Action::DeleteDialog(pos)) => self.delete_dialog = Some(((pos + 1).to_string(), None)),
                None => {}
            }
        }
        let mut anchors = CommentAnchors::default();
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::NONE.fill(palette.canvas))
            .show(ui, |ui| {
                if self.doc.is_some() {
                    anchors = self.pages(ui);
                } else {
                    self.welcome(ui);
                }
            });
        if self.doc.is_some() && !self.crop_mode && !self.redact_mode && !self.edit_mode {
            self.comment_overlays(&ctx, &anchors);
        }
        if self.crop_mode
            && self.doc.is_some()
            && let Some(action) = self.crop.panel(&ctx, self.view_rect, self.order.len())
        {
            self.handle_crop_action(action);
        }
        if self.rotate_mode
            && self.doc.is_some()
            && let Some(action) = self.rotate.panel(&ctx, self.view_rect, self.current_page, self.order.len())
        {
            match action {
                rotate::Action::Rotate { positions, degrees } => self.rotate_pages(&positions, degrees),
                rotate::Action::Done => self.rotate_mode = false,
            }
        }
        if self.redact_mode
            && self.doc.is_some()
            && let Some(action) = self.redact.panel(&ctx, self.view_rect)
        {
            self.handle_redact_action(action);
        }
        if self.doc.is_some() {
            self.page_pill(&ctx);
        }
        drop_highlight(&ctx);
        self.poll_print();
        if self.doc.is_some() {
            let name = self.doc.as_ref().and_then(|d| d.path.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let sizes: Vec<Vec2> = self.order.iter().map(|&k| self.doc.as_ref().map_or(Vec2::ZERO, |d| d.page_sizes[k])).collect();
            let (textures, order) = (&self.textures, &self.order);
            let texture = |pos: usize| order.get(pos).and_then(|k| textures.get(k)).map(|t| t.texture.id());
            let view = print::DocView { title: &name, sizes: &sizes, current: self.current_page, texture: &texture };
            if let Some(print::Action::Print(job)) = self.print.show(&ctx, &view) {
                self.start_print(job);
            }
        }
        self.poll_compress();
        match self.compress.show(&ctx) {
            Some(compress::Action::Start(options)) => self.start_compress(options),
            Some(compress::Action::Open(path)) => self.open(path),
            None => {}
        }
        self.delete_dialog(&ctx);
        self.info_window(&ctx);
        self.unsaved_prompt(&ctx);
        self.take_screenshot(&ctx);
    }
}

impl Viewer {
    /// `--screenshot`: once nothing is left to render, capture and exit.
    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.screenshot else { return };
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            for px in &image.pixels {
                ppm.extend_from_slice(&[px.r(), px.g(), px.b()]);
            }
            if let Err(e) = std::fs::write(&shot.path, ppm) {
                eprintln!("screenshot failed: {e}");
            }
            if shot.exit {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.screenshot = None;
                ctx.request_repaint();
            }
            return;
        }
        let done_loading = self.loading.is_none() && (self.doc.is_some() || self.error.is_some());
        let rendered = self.last_want.0.is_empty() && !self.textures.is_empty() && !self.sidebar.pending()
            || self.error.is_some();
        if done_loading && rendered && !shot.requested {
            if shot.settle > 0 {
                shot.settle -= 1;
            } else {
                shot.requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
        }
        ctx.request_repaint();
    }
}

/// What a redaction removed, in words.
/// The screens (monitors), in the windowing system's order, with the main
/// one marked: the primary screen, or else the one the viewer is on.
fn screens(frame: &eframe::Frame) -> Vec<present::Screen> {
    let Some(window) = frame.winit_window() else { return Vec::new() };
    let monitors: Vec<_> = window.available_monitors().collect();
    let main = window
        .primary_monitor()
        .and_then(|p| monitors.iter().position(|m| *m == p))
        .or_else(|| window.current_monitor().and_then(|c| monitors.iter().position(|m| *m == c)))
        .unwrap_or(0);
    let mut out: Vec<present::Screen> = Vec::new();
    let mut seen = Vec::new();
    for (i, m) in monitors.iter().enumerate() {
        // Some systems list a screen twice.
        let key = (m.name(), m.position().x, m.position().y, m.size().width, m.size().height);
        if seen.contains(&key) {
            if i == main
                && let Some(first) = seen.iter().position(|k| *k == key)
            {
                out[first].main = true;
            }
            continue;
        }
        seen.push(key);
        out.push(present::Screen {
            index: i,
            name: m.name().unwrap_or_else(|| format!("Screen {}", out.len() + 1)),
            size: (m.size().width, m.size().height),
            main: i == main,
        });
    }
    out
}

/// "1.24 MB (1,301,234 bytes)".
fn file_size(bytes: u64) -> String {
    let digits = bytes.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    if bytes < 1024 { format!("{grouped} bytes") } else { format!("{} ({grouped} bytes)", compress::size(bytes)) }
}

fn redaction_summary(done: &k_pdf::edit::Redacted) -> String {
    let count = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    let parts: Vec<String> = [
        count(done.chars, "character", "characters"),
        count(done.images + done.images_removed, "image", "images"),
        count(done.shapes, "shape", "shapes"),
        count(done.annotations, "link or note", "links and notes"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "blacked out (nothing was under the marks) — save to keep it".into()
    } else {
        format!("removed {} — save to keep it", parts.join(", "))
    }
}

/// A thin vertical line between groups of toolbar controls.
fn divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(9.0, 18.0), Sense::hover());
    let color = theme::palette(ui).border;
    ui.painter().vline(rect.center().x, rect.y_range(), egui::Stroke::new(1.0, color));
}

/// While a file is dragged over the window: show that dropping opens it.
fn drop_highlight(ctx: &egui::Context) {
    if ctx.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let p = theme::of(&ctx.global_style().visuals);
    let rect = ctx.content_rect().shrink(14.0);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
    painter.rect_filled(rect, 14.0, p.canvas.gamma_multiply(0.85));
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
    painter.extend(egui::Shape::dashed_line(&corners, egui::Stroke::new(2.0, p.accent), 10.0, 6.0));
    let icon = Rect::from_center_size(rect.center() - Vec2::new(0.0, 24.0), Vec2::splat(40.0));
    icons::paint(&painter, icon, Icon::Document, p.accent);
    painter.text(
        rect.center() + Vec2::new(0.0, 22.0),
        egui::Align2::CENTER_TOP,
        "Drop to open",
        egui::FontId::proportional(18.0),
        p.text,
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_sizes() {
        assert_eq!(super::file_size(512), "512 bytes");
        assert_eq!(super::file_size(1_301_234), "1.24 MB (1,301,234 bytes)");
        assert_eq!(super::file_size(45_000), "44 KB (45,000 bytes)");
    }
}
