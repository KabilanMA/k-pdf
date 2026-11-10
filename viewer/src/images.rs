//! Images (and PDF pages) opened as a document.
//!
//! Opening JPEG, PNG, GIF, BMP, TIFF or WebP images shows them like the
//! pages of a PDF; PDFs can be added too, each page becoming one of the
//! pages (kept as it is, or made into a cleaner scanned page): thumbnails in the sidebar (where more images are added,
//! and pages are reordered or removed) and the selected page large in the
//! middle. Each image has its own look, chosen in the bar above it: as it
//! is, or made into a scanned page, where the page is found in the photo,
//! flattened, straightened and cleaned up (`k_pdf::scan`). Saving writes
//! them all into one PDF, one page per image.
//!
//! Images are read in the background, in order. Each page keeps only small
//! textures (the image and its scanned page, for the thumbnails); the
//! pages looked at last also keep a larger copy (at most `SHOWN` pixels a
//! side, to show and to make previews from), read again from the file when
//! a page is looked at once more. The full images are read again when
//! saving.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use eframe::egui::{self, Color32, CursorIcon, Id, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Vec2};
use k_pdf::scan::{self, Image, Mode, Quad, ScanOptions};

use crate::icons::{self, Icon};
use crate::theme;

/// Pages are shown large, and previews made, from a copy whose longer side
/// is at most this.
const SHOWN: u32 = 1600;
/// The longer side of the previews of scanned pages shown large.
const PREVIEW: u32 = 1400;
/// The longer side of thumbnails, at most (and of the previews made for
/// them).
const THUMB: u32 = 480;
/// How many pages keep their large copy (about 6 MB each, and its texture).
const LARGE_KEPT: usize = 3;
/// PDF pages made into scanned pages are rendered this many pixels on
/// their longer side.
const PDF_PIXELS: f32 = scan::PDF_PAGE_PIXELS;
/// Steps Ctrl+Z can take back.
const MAX_UNDO: usize = 50;
/// Sidebar: space left and right of the thumbnails, under each, and
/// between them.
const PAD: f32 = 12.0;
const LABEL_HEIGHT: f32 = 18.0;
const GAP: f32 = 14.0;

pub use scan::IMAGE_EXTENSIONS;

/// Whether `path` is an image that can be opened (by its extension).
pub fn is_image(path: &Path) -> bool {
    scan::is_image_file(path)
}

/// Whether `path` is a PDF (by its extension).
pub fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// The extensions of the files that can be added: images and PDFs.
pub fn addable_extensions() -> Vec<&'static str> {
    let mut all = IMAGE_EXTENSIONS.to_vec();
    all.push("pdf");
    all
}

/// Renders a PDF page with its longer side `max` pixels, as RGB.
fn render_page(doc: &k_pdf::Document, renderer: &mut k_pdf::Renderer, page: &k_pdf::Page, max: f32) -> Result<Image, String> {
    scan::pdf_page_image(doc, renderer, page, max).map_err(|e| e.to_string())
}

/// Unsaved edits of the open document, shown on its pages.
type Overlay = Option<Arc<k_pdf::Changes>>;

/// Opens a PDF, with `overlay` (unsaved edits) when given.
fn open_pdf(path: &Path, overlay: &Overlay) -> Result<k_pdf::Document, String> {
    let doc = k_pdf::Document::open(path).map_err(|e| e.to_string())?;
    if let Some(changes) = overlay {
        doc.set_overlay(changes);
    }
    Ok(doc)
}

/// A page's picture at most `max` pixels a side: an image file, or page
/// `page` of a PDF.
fn read_picture(path: &Path, page: Option<usize>, overlay: &Overlay, max: u32) -> Result<Image, String> {
    let Some(page) = page else {
        return Image::open(path).map(|i| i.shrink(max).0.to_rgb()).map_err(|e| e.to_string());
    };
    let doc = open_pdf(path, overlay)?;
    let pages = doc.pages().map_err(|e| e.to_string())?;
    let p = pages.get(page).ok_or("the page is gone")?;
    let mut renderer = k_pdf::Renderer::new(&doc);
    render_page(&doc, &mut renderer, p, max as f32)
}

/// The size of the picture the quad is measured on: the image's, or a
/// PDF page's at `PDF_PIXELS`.
fn page_pixels(doc: &k_pdf::Document, page: &k_pdf::Page) -> [u32; 2] {
    let (w, h) = page.display_size(doc);
    let scale = PDF_PIXELS as f64 / w.max(h).max(1.0);
    [(w * scale).round().max(1.0) as u32, (h * scale).round().max(1.0) as u32]
}

/// How an image becomes a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Look {
    /// The whole image, unchanged.
    AsIs,
    /// The page found in the photo, flattened, then cleaned up like a scan
    /// (or, with `Mode::Original`, only cut out).
    Scan(Mode),
}

impl Look {
    const ALL: [Look; 5] =
        [Look::AsIs, Look::Scan(Mode::Original), Look::Scan(Mode::Color), Look::Scan(Mode::Gray), Look::Scan(Mode::BlackWhite)];

    fn label(self) -> &'static str {
        match self {
            Look::AsIs => "As is",
            Look::Scan(Mode::Original) => "Cropped",
            Look::Scan(Mode::Color) => "Color scan",
            Look::Scan(Mode::Gray) => "Gray scan",
            Look::Scan(Mode::BlackWhite) => "Black & white",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Look::AsIs => "The whole image, as it is",
            Look::Scan(Mode::Original) => "Only the page, cut out of the photo and flattened; colors as photographed",
            Look::Scan(Mode::Color) => "Like a color scan: the page flattened, shadows removed, paper white and ink crisp",
            Look::Scan(Mode::Gray) => "Like a grayscale scan: the page flattened, paper white and ink crisp",
            Look::Scan(Mode::BlackWhite) => "Pure black and white: the sharpest text and the smallest file",
        }
    }

    fn scanned(self) -> bool {
        self != Look::AsIs
    }
}

/// What an image's page looks like: the look, the page's corners in the
/// image (pixels of the full image, for scanned looks), quarter turns
/// clockwise, and whether slanted text is leveled.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    look: Look,
    quad: Quad,
    turns: u8,
    straighten: bool,
    /// Cleaned up for handwriting (see `ScanOptions::handwriting`).
    handwriting: bool,
}

#[derive(Clone)]
struct Picture {
    /// Tells pictures apart in messages (a file can be added twice).
    id: u64,
    path: PathBuf,
    /// The page of the PDF at `path`, for a PDF's page.
    page: Option<usize>,
    /// Cleaning up the open document: the page's key there, and its
    /// unsaved edits (for pages of the open file).
    key: Option<usize>,
    overlay: Overlay,
    name: String,
    /// The full image's size in pixels.
    size: Vec2,
    /// The image, small.
    thumb: egui::TextureHandle,
    /// The page as found in the image, if it was.
    found: Option<Quad>,
    settings: Settings,
    /// The scanned page, small, made for these settings (maybe older ones).
    preview: Option<(Settings, egui::TextureHandle)>,
}

impl Picture {
    fn whole(&self) -> Quad {
        [[0.0, 0.0], [self.size.x, 0.0], [self.size.x, self.size.y], [0.0, self.size.y]]
    }

    /// The page, small: a texture, the quarter turns to show it with, and
    /// the page's shape. `None` while a scanned page has no preview yet.
    fn small(&self) -> Option<(egui::TextureId, u8, Vec2)> {
        let s = &self.settings;
        if !s.look.scanned() {
            return Some((self.thumb.id(), s.turns, turned(self.size, s.turns)));
        }
        let (key, texture) = self.preview.as_ref().filter(|(k, _)| k.look.scanned())?;
        Some(stand_in(key, texture, s))
    }

    /// The page's shape (width and height, in proportion).
    fn shape(&self) -> Vec2 {
        if let Some((_, _, size)) = self.small() {
            return size;
        }
        // From the corners: the average of opposite sides.
        let q = self.settings.quad;
        let d = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        let size = Vec2::new((d(q[0], q[1]) + d(q[3], q[2])) / 2.0, (d(q[0], q[3]) + d(q[1], q[2])) / 2.0);
        turned(size.max(Vec2::splat(1.0)), self.settings.turns)
    }

    /// Whether the thumbnail of a scanned page is out of date.
    fn thumb_stale(&self) -> bool {
        stale(self.preview.as_ref(), &self.settings)
    }
}

/// Whether a preview was made for other settings than `s` (or is missing).
fn stale(preview: Option<&(Settings, egui::TextureHandle)>, s: &Settings) -> bool {
    s.look.scanned() && preview.is_none_or(|(k, _)| k != s)
}

/// A preview made for `key` standing in for one with settings `s` (while
/// that is made): turned to the current turns.
fn stand_in(key: &Settings, texture: &egui::TextureHandle, s: &Settings) -> (egui::TextureId, u8, Vec2) {
    let extra = (s.turns + 4 - key.turns % 4) % 4;
    (texture.id(), extra, turned(texture.size_vec2(), extra))
}

/// A page looked at lately: the image at most `SHOWN` pixels a side (what
/// previews are made from), its texture, and the scanned page, large.
struct Large {
    id: u64,
    image: Arc<Image>,
    texture: egui::TextureHandle,
    preview: Option<(Settings, egui::TextureHandle)>,
}

/// A size after `turns` quarter turns.
fn turned(size: Vec2, turns: u8) -> Vec2 {
    if turns % 2 == 1 { Vec2::new(size.y, size.x) } else { size }
}

/// A point of an image `size` pixels large, after the image is turned
/// clockwise by `turns` quarter turns.
fn turn_point(p: [f32; 2], size: Vec2, turns: u8) -> [f32; 2] {
    let (mut p, mut size) = (p, size);
    for _ in 0..turns % 4 {
        p = [size.y - p[1], p[0]];
        size = Vec2::new(size.y, size.x);
    }
    p
}

/// The point of the image (`size` pixels before turning) shown at `p` once
/// turned by `turns`.
fn unturn_point(p: [f32; 2], size: Vec2, turns: u8) -> [f32; 2] {
    let mut p = p;
    let mut shown = turned(size, turns);
    for _ in 0..turns % 4 {
        p = [p[1], shown.x - p[0]];
        shown = Vec2::new(shown.y, shown.x);
    }
    p
}

/// Draws `texture` (its `uv` part) into `rect`, turned clockwise by
/// `turns` quarter turns.
fn paint_turned(painter: &egui::Painter, texture: egui::TextureId, rect: Rect, uv: Rect, turns: u8, tint: Color32) {
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    let uvs = [uv.left_top(), uv.right_top(), uv.right_bottom(), uv.left_bottom()];
    let mut mesh = egui::Mesh::with_texture(texture);
    for (i, &pos) in corners.iter().enumerate() {
        mesh.vertices.push(egui::epaint::Vertex { pos, uv: uvs[(i + 4 - turns as usize % 4) % 4], color: tint });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

fn full_uv() -> Rect {
    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
}

/// Textures are drawn small in the sidebar too: mipmaps keep them smooth.
fn texture_options() -> egui::TextureOptions {
    egui::TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..egui::TextureOptions::LINEAR }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

enum Msg {
    /// An image, or a page of a PDF, read.
    Loaded {
        id: u64,
        at: usize,
        path: PathBuf,
        page: Option<usize>,
        key: Option<usize>,
        overlay: Overlay,
        size: [u32; 2],
        thumb: Image,
        shown: Image,
        found: Option<Quad>,
    },
    /// A file was read (all its pages), or couldn't be.
    FileDone,
    LoadFailed { path: PathBuf, error: String },
    /// A page's large copy, read again (or why it couldn't be).
    Shown { id: u64, image: Result<Image, String> },
    /// A scanned page, made for `key`: large (`SHOWN`) or for a thumbnail.
    Preview { id: u64, key: Settings, image: Image, large: bool },
    Progress(usize),
    Saved(Result<PathBuf, String>),
}

/// What happened, for the viewer.
pub enum Event {
    Saved,
    SaveFailed,
    /// No image could be read, so there is nothing to show.
    NothingRead(String),
}

pub struct Images {
    pages: Vec<Picture>,
    selected: usize,
    /// Files still being read.
    loading: usize,
    /// The next picture's id (taken by the threads that read them).
    next_id: Arc<std::sync::atomic::AtomicU64>,
    /// The look pages get when they are read.
    default_look: Look,
    default_handwriting: bool,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    /// Pages looked at lately, the last one last.
    large: Vec<Large>,
    /// The page whose large copy is being read again.
    reading: Option<u64>,
    /// A preview is being made.
    making: bool,
    /// Placing the corners of the selected page.
    corners: bool,
    /// The corner being dragged.
    drag: Option<usize>,
    /// The thumbnail being dragged to another place.
    moving: Option<usize>,
    /// Saving: pages done, of how many.
    saving: Option<(usize, usize)>,
    /// Where the PDF was last saved (Ctrl+S saves there again).
    pub saved_to: Option<PathBuf>,
    modified: bool,
    pub error: Option<String>,
    undo: Vec<(Vec<Picture>, usize)>,
    redo: Vec<(Vec<Picture>, usize)>,
    /// Height of the bar above the page, last frame (to keep the page
    /// clear of it).
    bar_height: f32,
    /// "Add images" was clicked: where to add them.
    add_request: Option<usize>,
    /// The pages are for inserting into the open document, at this
    /// position (instead of saving them as a PDF of their own).
    pub insert_at: Option<usize>,
    /// The pages are the open document's, being cleaned up: applying
    /// replaces the changed ones there (see [`Images::apply`]).
    pub cleaning: bool,
    /// After [`Images::apply`]: the keys of the pages made into new ones,
    /// in the order they are written.
    pub replaced: Vec<usize>,
    /// The "Apply to" field (page numbers), and why it can't be used.
    apply_pages: String,
    apply_error: Option<String>,
    /// The page to show once it has been read.
    start_at: Option<usize>,
}

/// Something to read: a file (all its pages), or one page of a PDF (of
/// the open document, with its key there and its unsaved edits).
pub struct Item {
    pub path: PathBuf,
    pub page: Option<usize>,
    pub key: Option<usize>,
    pub overlay: Option<Arc<k_pdf::Changes>>,
}

impl Images {
    /// Starts reading `paths` (opened as a new document).
    pub fn open(paths: Vec<PathBuf>) -> Images {
        let (tx, rx) = mpsc::channel();
        let mut images = Images {
            pages: Vec::new(),
            selected: 0,
            loading: 0,
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            default_look: Look::AsIs,
            default_handwriting: false,
            tx,
            rx,
            large: Vec::new(),
            reading: None,
            making: false,
            corners: false,
            drag: None,
            moving: None,
            saving: None,
            saved_to: None,
            modified: false,
            error: None,
            undo: Vec::new(),
            redo: Vec::new(),
            bar_height: 52.0,
            add_request: None,
            insert_at: None,
            cleaning: false,
            replaced: Vec::new(),
            apply_pages: String::new(),
            apply_error: None,
            start_at: None,
        };
        images.read(paths, 0);
        images
    }

    /// Starts reading `paths`, to be made into pages and inserted into the
    /// open document at position `at`.
    pub fn inserting(paths: Vec<PathBuf>, at: usize) -> Images {
        Images { insert_at: Some(at), ..Images::open(paths) }
    }

    /// Shows the open document's pages (`items`, in order) to clean up:
    /// they start as they are; choosing a scan look (with Handwriting on to
    /// begin with: it is mostly for handwritten pages) and applying
    /// replaces them in the document.
    pub fn cleaning(items: Vec<Item>) -> Images {
        let mut images = Images { default_handwriting: true, cleaning: true, ..Images::open(Vec::new()) };
        images.read_items(items, 0);
        images
    }

    /// Cleaning up: the pages to make again (writing them to `target` in
    /// the background; their keys go to `replaced`) and the pages kept as
    /// they are but turned: (key, quarter turns).
    pub fn apply(&mut self, target: PathBuf) -> Vec<(usize, u8)> {
        let turned = self
            .pages
            .iter()
            .filter(|p| p.settings.look == Look::AsIs && p.settings.turns % 4 != 0)
            .filter_map(|p| Some((p.key?, p.settings.turns % 4)))
            .collect();
        let made: Vec<&Picture> = self.pages.iter().filter(|p| p.settings.look != Look::AsIs).collect();
        self.replaced = made.iter().filter_map(|p| p.key).collect();
        if !made.is_empty() {
            let jobs: Vec<Job> = made.iter().map(|p| (p.path.clone(), p.page, p.settings, p.overlay.clone())).collect();
            self.error = None;
            self.saving = Some((0, jobs.len()));
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write_pages(&jobs, &target, &tx)))
                    .unwrap_or_else(|_| Err("something went wrong while making the pages".into()));
                tx.send(Msg::Saved(result.map(|()| target))).ok();
            });
        }
        turned
    }

    /// Shows the page at `position` (when it has been read: until then,
    /// the first).
    pub fn select_position(&mut self, position: usize) {
        self.start_at = Some(position);
    }

    /// Whether there is anything to apply (cleaning up).
    pub fn has_changes(&self) -> bool {
        self.pages.iter().any(|p| p.settings.look != Look::AsIs || p.settings.turns % 4 != 0)
    }

    /// Gives the pages at `positions` the selected page's look,
    /// straightening and handwriting cleanup.
    fn apply_look_to(&mut self, positions: &[usize]) {
        let Some(s) = self.pages.get(self.selected).map(|p| p.settings) else { return };
        let same = |p: &Picture| p.settings.look == s.look && p.settings.straighten == s.straighten && p.settings.handwriting == s.handwriting;
        if positions.iter().all(|&i| self.pages.get(i).is_none_or(same)) {
            return;
        }
        self.remember();
        self.modified = true;
        for &i in positions {
            if let Some(p) = self.pages.get_mut(i) {
                p.settings.look = s.look;
                p.settings.straighten = s.straighten;
                p.settings.handwriting = s.handwriting;
            }
        }
    }

    /// Reads `paths` in the background, one after another (each page of a
    /// PDF), and puts them at position `at` in that order.
    fn read(&mut self, paths: Vec<PathBuf>, at: usize) {
        self.read_items(paths.into_iter().map(|path| Item { path, page: None, key: None, overlay: None }).collect(), at);
    }

    /// Reads `items` in the background, one after another, and puts them at
    /// position `at` in that order.
    fn read_items(&mut self, items: Vec<Item>, at: usize) {
        // (Pages of the open document count as one file, done once they
        // have all been read, however many there are.)
        let document = items.iter().any(|i| i.key.is_some());
        self.loading += if document { 1 } else { items.len() };
        let (ids, tx) = (self.next_id.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let mut at = at;
            let mut failed = false;
            for item in &items {
                let Item { path, page: only, key, overlay } = item;
                let send = |msg: Msg| tx.send(msg).is_ok();
                // One picture: sent at the next position.
                let mut loaded = |page: Option<usize>, size: [u32; 2], image: &Image, found: Option<Quad>| {
                    let id = ids.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let thumb = image.shrink(THUMB).0.to_rgb();
                    let shown = image.shrink(SHOWN).0.to_rgb();
                    at += 1;
                    let (path, key, overlay) = (path.clone(), *key, overlay.clone());
                    send(Msg::Loaded { id, at: at - 1, path, page, key, overlay, size, thumb, shown, found })
                };
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<bool, String> {
                    if !is_pdf(path) {
                        let image = Image::open(path).map_err(|e| e.to_string())?;
                        let found = scan::find_page(&image);
                        return Ok(loaded(None, [image.width, image.height], &image, found));
                    }
                    let doc = open_pdf(path, overlay)?;
                    if doc.is_encrypted() {
                        return Err("encrypted PDFs can't be read yet".into());
                    }
                    let pages = doc.pages().map_err(|e| e.to_string())?;
                    let mut renderer = k_pdf::Renderer::new(&doc);
                    for (i, page) in pages.iter().enumerate() {
                        if only.is_some_and(|p| p != i) {
                            continue;
                        }
                        let size = page_pixels(&doc, page);
                        let image = render_page(&doc, &mut renderer, page, SHOWN as f32)?;
                        // The page as found, measured on the full-size page.
                        let k = size[0] as f32 / image.width as f32;
                        let found = scan::find_page(&image).map(|q| q.map(|[x, y]| [x * k, y * k]));
                        if !loaded(Some(i), size, &image, found) {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }));
                let sent = match read {
                    Ok(Ok(true)) => key.is_some() || send(Msg::FileDone),
                    Ok(Ok(false)) => false,
                    Ok(Err(error)) => {
                        failed = true;
                        send(Msg::LoadFailed { path: path.clone(), error })
                    }
                    Err(_) => {
                        failed = true;
                        send(Msg::LoadFailed { path: path.clone(), error: "something went wrong while reading it".into() })
                    }
                };
                if !sent {
                    return; // the images were closed
                }
            }
            if document && !failed {
                tx.send(Msg::FileDone).ok();
            }
        });
    }

    /// Asks the viewer for images to add at `at` (it shows the file
    /// dialog, then calls [`Images::add`]).
    fn choose_and_add(&mut self, at: usize) {
        self.add_request = Some(at);
    }

    /// Where images were asked to be added, if they were.
    pub fn take_add_request(&mut self) -> Option<usize> {
        self.add_request.take()
    }

    /// Adds images at `at` (or at the end).
    pub fn add(&mut self, paths: Vec<PathBuf>, at: Option<usize>) {
        self.remember();
        self.modified = true;
        self.read(paths, at.unwrap_or(self.pages.len()));
    }

    /// The folder of the first image (to save next to it).
    pub fn folder(&self) -> Option<PathBuf> {
        self.pages.first().and_then(|p| p.path.parent()).map(Path::to_path_buf)
    }

    /// A name for the PDF: the saved file's, or the first image's.
    pub fn suggested_name(&self) -> String {
        if let Some(path) = &self.saved_to {
            return file_name(path);
        }
        let first = self.pages.first();
        let stem = first.and_then(|p| p.path.file_stem()).map(|s| s.to_string_lossy().into_owned());
        // (Not over a PDF the pages come from.)
        let suffix = if first.is_some_and(|p| p.page.is_some()) { "-scanned" } else { "" };
        format!("{}{suffix}.pdf", stem.unwrap_or_else(|| "Images".into()))
    }

    /// The title: the saved PDF's name, or the image's (or how many).
    pub fn title(&self) -> String {
        match (&self.saved_to, self.pages.as_slice()) {
            (Some(path), _) => file_name(path),
            (None, [one]) => one.name.clone(),
            // All pages of one PDF.
            (None, [first, ..]) if self.loading == 0 && self.pages.iter().all(|p| p.path == first.path) => file_name(&first.path),
            (None, pages) => format!("{} images", pages.len() + self.loading),
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Whether there are changes that aren't in a saved PDF: anything done
    /// after opening, or after saving.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    /// Whether images are being read or saved, or the page shown is being
    /// made (while placing corners, the photo is shown instead).
    pub fn busy(&self) -> bool {
        let shown = self.pages.get(self.selected).is_none_or(|p| match self.large(p.id) {
            None => false,
            Some(large) => self.corners || !stale(large.preview.as_ref(), &p.settings),
        });
        self.loading > 0 || self.saving.is_some() || !shown
    }

    fn large(&self, id: u64) -> Option<&Large> {
        self.large.iter().find(|l| l.id == id)
    }

    /// Keeps a page's large copy, forgetting the ones looked at longest ago
    /// (but not the selected page's).
    fn keep_large(&mut self, ctx: &egui::Context, id: u64, image: Image) {
        let selected = self.pages.get(self.selected).map(|p| p.id);
        if self.large(id).is_some() || (self.large.len() >= LARGE_KEPT && selected != Some(id)) {
            return;
        }
        let pixels = egui::ColorImage::from_rgb([image.width as usize, image.height as usize], &image.data);
        let texture = ctx.load_texture(format!("image-large-{id}"), pixels, texture_options());
        self.large.push(Large { id, image: Arc::new(image), texture, preview: None });
        while self.large.len() > LARGE_KEPT {
            match self.large.iter().position(|l| Some(l.id) != selected) {
                Some(i) => drop(self.large.remove(i)),
                None => break,
            }
        }
    }

    /// What to draw for page `i`, large: a texture, the quarter turns to
    /// show it with, the page's shape, and whether a newer one is being
    /// made. `None` while a scanned page has no preview yet.
    fn big(&self, i: usize) -> Option<(egui::TextureId, u8, Vec2, bool)> {
        let p = self.pages.get(i)?;
        let s = &p.settings;
        let large = self.large(p.id);
        if !s.look.scanned() {
            let texture = large.map_or(p.thumb.id(), |l| l.texture.id());
            return Some((texture, s.turns, turned(p.size, s.turns), false));
        }
        let stale = large.is_none_or(|l| stale(l.preview.as_ref(), s));
        let (key, texture) =
            large.and_then(|l| l.preview.as_ref()).or(p.preview.as_ref()).filter(|(k, _)| k.look.scanned())?;
        let (texture, turns, shape) = stand_in(key, texture, s);
        Some((texture, turns, shape, stale))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Keeps the pages as they are, for Ctrl+Z.
    fn remember(&mut self) {
        self.undo.push((self.pages.clone(), self.selected));
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) {
        if let Some((pages, selected)) = self.undo.pop() {
            self.redo.push((std::mem::replace(&mut self.pages, pages), self.selected));
            self.selected = selected.min(self.pages.len().saturating_sub(1));
            self.modified = true;
            self.drag = None;
        }
    }

    pub fn redo(&mut self) {
        if let Some((pages, selected)) = self.redo.pop() {
            self.undo.push((std::mem::replace(&mut self.pages, pages), self.selected));
            self.selected = selected.min(self.pages.len().saturating_sub(1));
            self.modified = true;
            self.drag = None;
        }
    }

    fn select(&mut self, i: usize) {
        if i < self.pages.len() && i != self.selected {
            self.selected = i;
            self.corners = false;
            self.drag = None;
            // Looked at last: kept longest.
            let id = self.pages[i].id;
            if let Some(k) = self.large.iter().position(|l| l.id == id) {
                let large = self.large.remove(k);
                self.large.push(large);
            }
        }
    }

    fn remove(&mut self, i: usize) {
        if i >= self.pages.len() {
            return;
        }
        self.remember();
        self.modified = true;
        self.pages.remove(i);
        if self.selected > i || self.selected >= self.pages.len() {
            self.selected = self.selected.saturating_sub(1);
        }
        self.corners = false;
    }

    /// Moves the page at `from` into the gap before position `to`.
    fn move_page(&mut self, from: usize, to: usize) {
        let to = if to > from { to - 1 } else { to };
        if from >= self.pages.len() || to == from {
            return;
        }
        self.remember();
        self.modified = true;
        let selected = self.pages[self.selected].id;
        let page = self.pages.remove(from);
        self.pages.insert(to.min(self.pages.len()), page);
        self.selected = self.pages.iter().position(|p| p.id == selected).unwrap_or(0);
    }

    /// Changes the selected page's settings (one undo step).
    fn change(&mut self, change: impl FnOnce(&mut Picture)) {
        let Some(page) = self.pages.get(self.selected) else { return };
        let mut changed = page.clone();
        change(&mut changed);
        if changed.settings != page.settings {
            self.remember();
            self.modified = true;
            self.pages[self.selected] = changed;
        }
    }

    fn set_look(&mut self, look: Look) {
        // (A PDF's page is the whole sheet already.)
        let found = self.pages.get(self.selected).is_some_and(|p| p.found.is_some() || p.page.is_some());
        self.change(|p| p.settings.look = look);
        // No page found in the photo: show where its corners go.
        self.corners = look.scanned() && (self.corners || !found);
    }

    pub fn turn(&mut self, clockwise: bool) {
        self.change(|p| p.settings.turns = (p.settings.turns + if clockwise { 1 } else { 3 }) % 4);
    }

    /// Handles finished background work.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Event> {
        let mut event = None;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Loaded { id, at, path, page, key, overlay, size, thumb, shown, found } => {
                    let image = egui::ColorImage::from_rgb([thumb.width as usize, thumb.height as usize], &thumb.data);
                    let thumb = ctx.load_texture(format!("image-{id}"), image, texture_options());
                    let size = Vec2::new(size[0] as f32, size[1] as f32);
                    let whole = [[0.0, 0.0], [size.x, 0.0], [size.x, size.y], [0.0, size.y]];
                    // A PDF's page is usually the whole sheet already (a
                    // scan): the corners start on its edges ("Find page"
                    // puts them where a page was found, for photos).
                    let quad = if page.is_some() { whole } else { found.unwrap_or(whole) };
                    let settings = Settings { look: self.default_look, quad, turns: 0, straighten: true, handwriting: self.default_handwriting };
                    let name = match page {
                        Some(i) => format!("{}, page {}", file_name(&path), i + 1),
                        None => file_name(&path),
                    };
                    let picture = Picture {
                        id,
                        name,
                        path,
                        page,
                        key,
                        overlay,
                        size,
                        thumb,
                        found,
                        settings,
                        preview: None,
                    };
                    let at = at.min(self.pages.len());
                    if at <= self.selected && !self.pages.is_empty() {
                        self.selected += 1;
                    }
                    self.pages.insert(at, picture);
                    if self.start_at == Some(at) {
                        self.start_at = None;
                        self.selected = at;
                    }
                    self.keep_large(ctx, id, shown);
                }
                Msg::Shown { id, image } => {
                    self.reading = None;
                    match image {
                        Ok(image) => self.keep_large(ctx, id, image),
                        Err(e) => {
                            // Don't try again and again.
                            self.error = Some(format!("Could not read the image again: {e}."));
                            let p = self.pages.iter().find(|p| p.id == id).map(|p| p.thumb.clone());
                            if let Some(texture) = p {
                                let image = Image { width: 1, height: 1, channels: 3, data: vec![255; 3] };
                                self.large.push(Large { id, image: Arc::new(image), texture, preview: None });
                            }
                        }
                    }
                }
                Msg::FileDone => {
                    self.loading = self.loading.saturating_sub(1);
                    if self.pages.is_empty() && self.loading == 0 {
                        event = Some(Event::NothingRead(self.error.take().unwrap_or_else(|| "There are no pages to show.".into())));
                    }
                }
                Msg::LoadFailed { path, error } => {
                    self.loading = self.loading.saturating_sub(1);
                    self.error = Some(format!("Could not open {}: {error}.", file_name(&path)));
                    if self.pages.is_empty() && self.loading == 0 {
                        event = self.error.take().map(Event::NothingRead);
                    }
                }
                Msg::Preview { id, key, image, large } => {
                    self.making = false;
                    let texture = |name: &str, image: &Image| {
                        let rgb = image.to_rgb();
                        let pixels = egui::ColorImage::from_rgb([rgb.width as usize, rgb.height as usize], &rgb.data);
                        ctx.load_texture(format!("{name}-{id}"), pixels, texture_options())
                    };
                    if large && let Some(l) = self.large.iter_mut().find(|l| l.id == id) {
                        l.preview = Some((key, texture("image-preview-large", &image)));
                    }
                    // The thumbnail too. (Earlier states, for undo, keep
                    // their own.)
                    if let Some(p) = self.pages.iter_mut().find(|p| p.id == id) {
                        let small = if large { image.shrink(THUMB).0 } else { image };
                        p.preview = Some((key, texture("image-preview", &small)));
                    }
                }
                Msg::Progress(done) => {
                    if let Some(s) = &mut self.saving {
                        s.0 = done;
                    }
                }
                Msg::Saved(result) => {
                    self.saving = None;
                    match result {
                        Ok(path) => {
                            self.saved_to = Some(path);
                            self.modified = false;
                            event = Some(Event::Saved);
                        }
                        Err(e) => {
                            self.error = Some(format!("Could not save the PDF: {e}."));
                            event = Some(Event::SaveFailed);
                        }
                    }
                }
            }
        }
        self.request_work();
        if self.busy() || self.making || self.reading.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        event
    }

    /// Starts what the pages shown need, one thing at a time: the
    /// selected page's large copy, its scanned page, then the other
    /// scanned pages' thumbnails.
    fn request_work(&mut self) {
        if self.drag.is_some() {
            return;
        }
        let Some(p) = self.pages.get(self.selected) else { return };
        let id = p.id;
        if self.large(id).is_none() {
            if self.reading.is_none() {
                self.reading = Some(id);
                let (path, page, overlay, tx) = (p.path.clone(), p.page, p.overlay.clone(), self.tx.clone());
                std::thread::spawn(move || {
                    let image = std::panic::catch_unwind(|| read_picture(&path, page, &overlay, SHOWN))
                        .unwrap_or_else(|_| Err("something went wrong while reading it".into()));
                    tx.send(Msg::Shown { id, image }).ok();
                });
            }
            return;
        }
        if self.making {
            return;
        }
        // The selected page's scanned page, large; or else a thumbnail.
        let large = self.large(id).filter(|l| stale(l.preview.as_ref(), &p.settings));
        let (p, source, max, large) = match large {
            Some(l) => (p, Some(l.image.clone()), PREVIEW, true),
            None => {
                let Some(p) = self.pages.iter().find(|q| q.thumb_stale()) else { return };
                (p, self.large(p.id).map(|l| l.image.clone()), THUMB, false)
            }
        };
        let key = p.settings;
        let Look::Scan(mode) = key.look else { return };
        self.making = true;
        let (id, path, page, size, tx) = (p.id, p.path.clone(), p.page, p.size, self.tx.clone());
        let overlay = p.overlay.clone();
        std::thread::spawn(move || {
            let make = || -> Option<Image> {
                let shown = match source {
                    Some(image) => image,
                    None => Arc::new(read_picture(&path, page, &overlay, SHOWN).ok()?),
                };
                let k = shown.width as f32 / size.x;
                let quad = key.quad.map(|[x, y]| [x * k, y * k]);
                let options = ScanOptions {
                    mode,
                    straighten: key.straighten,
                    turns: key.turns,
                    handwriting: key.handwriting,
                    max_size: max,
                    ..ScanOptions::new(quad)
                };
                Some(scan::scan(&shown, &options).image)
            };
            let image = std::panic::catch_unwind(make)
                .ok()
                .flatten()
                .unwrap_or_else(|| Image { width: 1, height: 1, channels: 1, data: vec![255] });
            tx.send(Msg::Preview { id, key, image, large }).ok();
        });
    }

    /// Writes every page into a PDF at `target`, in the background. Pages
    /// of PDFs kept as they are are copied as they are (only turned);
    /// everything else is made into image pages.
    pub fn save(&mut self, target: PathBuf) {
        if self.saving.is_some() || self.pages.is_empty() {
            return;
        }
        let jobs: Vec<Job> = self.pages.iter().map(|p| (p.path.clone(), p.page, p.settings, p.overlay.clone())).collect();
        let made = jobs.iter().filter(|j| !kept(j)).count();
        self.error = None;
        self.saving = Some((0, made.max(1)));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            // A bug in scanning must not leave the app waiting forever.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write_pages(&jobs, &target, &tx)))
                .unwrap_or_else(|_| Err("something went wrong while making the pages".into()));
            tx.send(Msg::Saved(result.map(|()| target))).ok();
        });
    }

    // ------------------------------------------------------------------
    // Keys and toolbar
    // ------------------------------------------------------------------

    /// Keys for going through the pages and changing them (when nothing
    /// is being typed).
    pub fn keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(Modifiers::NONE, Key::Escape) {
            self.corners = false;
            self.drag = None;
        }
        let last = self.pages.len().saturating_sub(1);
        if pressed(Modifiers::NONE, Key::ArrowLeft) || pressed(Modifiers::NONE, Key::ArrowUp) || pressed(Modifiers::NONE, Key::PageUp) {
            self.select(self.selected.saturating_sub(1));
        }
        if pressed(Modifiers::NONE, Key::ArrowRight) || pressed(Modifiers::NONE, Key::ArrowDown) || pressed(Modifiers::NONE, Key::PageDown)
        {
            self.select((self.selected + 1).min(last));
        }
        if pressed(Modifiers::NONE, Key::Home) {
            self.select(0);
        }
        if pressed(Modifiers::NONE, Key::End) {
            self.select(last);
        }
        if !self.cleaning && pressed(Modifiers::NONE, Key::Delete) {
            self.remove(self.selected);
        }
        // (Shift first: Ctrl+R would also match Ctrl+Shift+R.)
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::R) {
            self.turn(false);
        }
        if pressed(Modifiers::COMMAND, Key::R) {
            self.turn(true);
        }
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || pressed(Modifiers::COMMAND, Key::Y) {
            self.redo();
        }
        if pressed(Modifiers::COMMAND, Key::Z) {
            self.undo();
        }
    }

    /// The toolbar's buttons after Open and Save (`undo`: with undo and
    /// redo, which move into the "⋯" menu when the window is narrow).
    pub fn toolbar(&mut self, ui: &mut egui::Ui, labels: bool, undo: bool) {
        if undo {
            if icons::button(ui, Icon::Undo, None, false, self.can_undo()).on_hover_text("Undo (Ctrl+Z)").clicked() {
                self.undo();
            }
            if icons::button(ui, Icon::Redo, None, false, self.can_redo()).on_hover_text("Redo (Ctrl+Shift+Z or Ctrl+Y)").clicked() {
                self.redo();
            }
            crate::divider(ui);
        }
        if !self.cleaning
            && icons::button(ui, Icon::Plus, labels.then_some("Add images"), false, self.saving.is_none())
                .on_hover_text("Add more images or PDFs after this page")
                .clicked()
        {
            self.choose_and_add((self.selected + 1).min(self.pages.len()));
        }
        let any = !self.pages.is_empty();
        icons::group(ui, |ui| {
            if icons::button(ui, Icon::RotateLeft, None, false, any).on_hover_text("Turn left (Ctrl+Shift+R)").clicked() {
                self.turn(false);
            }
            if icons::button(ui, Icon::RotateRight, None, false, any).on_hover_text("Turn right (Ctrl+R)").clicked() {
                self.turn(true);
            }
            if !self.cleaning && icons::button(ui, Icon::Trash, None, false, any).on_hover_text("Remove this page (Delete)").clicked() {
                self.remove(self.selected);
            }
        });
    }

    /// The status line.
    pub fn status(&self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        if let Some((done, total)) = self.saving {
            ui.spinner();
            let doing = if self.insert_at.is_some() { "Making" } else { "Saving" };
            ui.label(format!("{doing} page {} of {total}…", (done + 1).min(total)));
        } else if let Some(e) = &self.error {
            ui.colored_label(p.danger, e);
        } else if self.loading > 0 {
            ui.spinner();
            ui.label(format!("Opening {} image{}…", self.loading, if self.loading == 1 { "" } else { "s" }));
        } else if self.corners {
            ui.label("Drag the corners onto the page's corners — Esc when done");
        } else if self.cleaning {
            ui.label("Pick a look for the pages to clean up (Apply to… for several), then Apply — pages left As is stay unchanged");
        } else if let Some(at) = self.insert_at {
            let place = if at == 0 { "at the start".to_string() } else { format!("after page {at}") };
            ui.label(format!("Choose how each image looks, then Insert adds them to the document {place}"));
        } else if self.modified {
            ui.label("Not saved yet — Ctrl+S saves the pages as one PDF");
        } else if let Some(path) = &self.saved_to {
            ui.label(format!("Saved as {} — Open PDF shows it in the viewer", path.display()));
        } else {
            ui.label("Choose how each image looks, then save them as one PDF (Ctrl+S)");
        }
    }

    // ------------------------------------------------------------------
    // Sidebar
    // ------------------------------------------------------------------

    /// The page thumbnails, to pick, reorder and remove pages, and a
    /// button to add images.
    pub fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let (header, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::hover());
        ui.painter().hline(header.x_range(), header.bottom() - 0.5, Stroke::new(1.0, p.border));
        let inner = header.shrink2(Vec2::new(PAD - 2.0, 4.0));
        let mut add_at = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            if !self.cleaning
                && icons::button(ui, Icon::Plus, None, false, self.saving.is_none()).on_hover_text("Add images or PDFs at the end").clicked()
            {
                add_at = Some(self.pages.len());
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("Pages").size(13.0).color(p.weak));
            });
        });

        let mut action: Option<SideAction> = None;
        let (pointer, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released()));
        egui::ScrollArea::vertical().id_salt("image-thumbnails").auto_shrink(false).show(ui, |ui| {
            let width = ui.available_width();
            let thumb_w = (width - 2.0 * PAD).max(20.0);
            ui.add_space(GAP);
            let mut items: Vec<Rect> = Vec::with_capacity(self.pages.len());
            for (i, page) in self.pages.iter().enumerate() {
                let shape = page.shape();
                let h = (thumb_w * shape.y / shape.x.max(1.0)).clamp(20.0, thumb_w * 2.0);
                let (item, response) = ui.allocate_exact_size(Vec2::new(width, h + LABEL_HEIGHT + GAP), Sense::click_and_drag());
                items.push(item);
                // The page drawn as large as fits, keeping its shape.
                let area = Rect::from_min_size(item.min + Vec2::new(PAD, 0.0), Vec2::new(thumb_w, h));
                let scale = (area.width() / shape.x).min(area.height() / shape.y);
                let rect = Rect::from_center_size(area.center(), shape * scale);
                if !ui.is_rect_visible(item) {
                    continue;
                }
                let painter = ui.painter().clone();
                let selected = i == self.selected;
                if response.hovered() && self.moving.is_none() {
                    painter.rect_filled(item.shrink2(Vec2::new(4.0, 0.0)).with_min_y(item.top() - 6.0), 8.0, p.hover);
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let tint = if self.moving == Some(i) { Color32::from_white_alpha(70) } else { Color32::WHITE };
                painter.add(egui::epaint::Shadow { offset: [0, 1], blur: 6, spread: 0, color: p.shadow }.as_shape(rect, 1));
                match page.small() {
                    Some((texture, turns, _)) => paint_turned(&painter, texture, rect, full_uv(), turns, tint),
                    None => {
                        painter.rect_filled(rect, 0.0, Color32::WHITE);
                        // The scanned page is on its way.
                        let spin = Rect::from_center_size(rect.center(), Vec2::splat(16.0));
                        ui.put(spin, egui::Spinner::new().size(16.0));
                    }
                }
                if selected {
                    painter.rect_stroke(rect.expand(2.0), 4.0, Stroke::new(2.0, p.accent), StrokeKind::Outside);
                }
                // The page number; the selected one's on an accent pill.
                let number = painter.layout_no_wrap((i + 1).to_string(), egui::FontId::proportional(11.5), p.text);
                let at = Pos2::new(rect.center().x, area.bottom() + 5.0 + number.size().y / 2.0);
                if selected {
                    painter.rect_filled(Rect::from_center_size(at, number.size() + Vec2::new(14.0, 3.0)), 8.0, p.accent);
                    painter.galley_with_override_text_color(at - number.size() / 2.0, number, p.on_accent);
                } else {
                    painter.galley_with_override_text_color(at - number.size() / 2.0, number, p.weak);
                }
                let response = response.on_hover_text(&page.name);
                if response.clicked() {
                    action = Some(SideAction::Select(i));
                }
                if response.drag_started() && !self.cleaning {
                    self.moving = Some(i);
                }
                // (Cleaning up, pages are added, moved and removed in the
                // document itself.)
                if !self.cleaning {
                    response.context_menu(|ui| {
                    if ui.button("Remove this page").clicked() {
                        action = Some(SideAction::Remove(i));
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("Move up")).clicked() {
                        action = Some(SideAction::Move(i, i - 1));
                    }
                    if ui.add_enabled(i + 1 < self.pages.len(), egui::Button::new("Move down")).clicked() {
                        action = Some(SideAction::Move(i, i + 2));
                    }
                    ui.separator();
                    if ui.button("Add images before this page…").clicked() {
                        action = Some(SideAction::Add(i));
                    }
                    if ui.button("Add images after this page…").clicked() {
                        action = Some(SideAction::Add(i + 1));
                    }
                    });
                }
                // A remove button on the hovered thumbnail.
                if !self.cleaning && self.moving.is_none() && ui.rect_contains_pointer(item) {
                    let button = Rect::from_center_size(rect.right_top() + Vec2::new(-14.0, 14.0), Vec2::splat(24.0));
                    let remove =
                        ui.interact(button, ui.id().with(("image-remove", page.id)), Sense::click()).on_hover_text("Remove this page (Ctrl+Z undoes it)");
                    let hot = remove.hovered();
                    let painter = ui.painter();
                    painter.circle(button.center(), 12.0, if hot { p.danger } else { p.raised }, Stroke::new(1.0, p.border));
                    icons::paint(painter, Rect::from_center_size(button.center(), Vec2::splat(14.0)), Icon::Trash, if hot { Color32::WHITE } else { p.text });
                    if remove.clicked() {
                        action = Some(SideAction::Remove(i));
                    }
                }
            }
            // Images still being read.
            for _ in 0..self.loading {
                let (item, _) = ui.allocate_exact_size(Vec2::new(width, thumb_w * 1.3 + LABEL_HEIGHT + GAP), Sense::hover());
                let rect = Rect::from_min_size(item.min + Vec2::new(PAD, 0.0), Vec2::new(thumb_w, thumb_w * 1.3));
                ui.painter().rect_filled(rect, 4.0, p.hover);
                ui.put(Rect::from_center_size(rect.center(), Vec2::splat(18.0)), egui::Spinner::new().size(18.0));
            }
            // A tile to add more.
            if self.cleaning {
                return;
            }
            let (tile, response) = ui.allocate_exact_size(Vec2::new(width, 72.0), Sense::click());
            let tile = tile.shrink2(Vec2::new(PAD, 4.0));
            let hot = response.hovered() && self.saving.is_none();
            ui.painter().rect(
                tile,
                8.0,
                if hot { p.hover } else { Color32::TRANSPARENT },
                Stroke::new(1.0, if hot { p.accent } else { p.border }),
                StrokeKind::Inside,
            );
            let color = if hot { p.accent } else { p.weak };
            icons::paint(ui.painter(), Rect::from_center_size(tile.center() - Vec2::new(0.0, 10.0), Vec2::splat(18.0)), Icon::Plus, color);
            ui.painter().text(tile.center() + Vec2::new(0.0, 14.0), egui::Align2::CENTER_CENTER, "Add images or PDFs", egui::FontId::proportional(12.5), color);
            if hot {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if response.clicked() && self.saving.is_none() {
                action = Some(SideAction::Add(self.pages.len()));
            }
            ui.add_space(GAP);

            // Dragging a thumbnail: a line where it would go.
            if let (Some(from), Some(pos)) = (self.moving, pointer) {
                let gap = items.iter().position(|r| pos.y < r.center().y).unwrap_or(items.len());
                let y = match items.get(gap) {
                    Some(r) => r.top() - GAP / 2.0,
                    None => items.last().map_or(0.0, |r| r.bottom() - GAP / 2.0),
                };
                if gap != from && gap != from + 1 {
                    let x = ui.max_rect().x_range();
                    ui.painter().line_segment(
                        [Pos2::new(x.min + PAD, y), Pos2::new(x.max - PAD, y)],
                        Stroke::new(2.0, p.accent),
                    );
                }
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                if released {
                    action = Some(SideAction::Move(from, gap));
                }
            }
        });
        if released {
            self.moving = None;
        }
        match action.or(add_at.map(SideAction::Add)) {
            Some(SideAction::Select(i)) => self.select(i),
            Some(SideAction::Remove(i)) => self.remove(i),
            Some(SideAction::Move(from, to)) => self.move_page(from, to),
            Some(SideAction::Add(at)) => self.choose_and_add(at),
            None => {}
        }
    }

    // ------------------------------------------------------------------
    // The page
    // ------------------------------------------------------------------

    /// The selected page, large, with the bar for its look above and page
    /// navigation below.
    pub fn view(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let view = ui.max_rect();
        let ctx = ui.ctx().clone();
        if self.pages.is_empty() {
            self.empty(ui, view);
            return;
        }
        let area = Rect::from_min_max(
            Pos2::new(view.left() + 24.0, view.top() + self.bar_height + 24.0),
            Pos2::new(view.right() - 24.0, view.bottom() - 66.0),
        );
        let scanned = self.pages[self.selected].settings.look.scanned();
        if self.corners && scanned && area.is_positive() {
            self.corner_editor(ui, area);
        } else if area.is_positive() {
            let big = self.big(self.selected);
            let shape = big.map_or_else(|| self.pages[self.selected].shape(), |b| b.2);
            let scale = (area.width() / shape.x).min(area.height() / shape.y);
            let rect = Rect::from_center_size(area.center(), shape * scale);
            let painter = ui.painter().clone();
            painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 14, spread: 0, color: p.shadow }.as_shape(rect, 1));
            match big {
                Some((texture, turns, _, stale)) => {
                    paint_turned(&painter, texture, rect, full_uv(), turns, Color32::WHITE);
                    if stale {
                        painter.rect_filled(rect, 0.0, Color32::from_white_alpha(70));
                        ui.put(Rect::from_center_size(rect.center(), Vec2::splat(28.0)), egui::Spinner::new().size(28.0));
                    }
                }
                None => {
                    painter.rect_filled(rect, 0.0, Color32::WHITE);
                    ui.put(Rect::from_center_size(rect.center(), Vec2::splat(28.0)), egui::Spinner::new().size(28.0));
                }
            }
        }
        self.look_bar(&ctx, view);
        self.page_pill(&ctx, view);
    }

    /// Nothing to show: still reading, or every page was removed.
    fn empty(&mut self, ui: &mut egui::Ui, view: Rect) {
        let p = theme::palette(ui);
        if self.loading > 0 {
            ui.put(Rect::from_center_size(view.center(), Vec2::splat(32.0)), egui::Spinner::new().size(32.0));
            return;
        }
        let mut add = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(view), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((view.height() / 2.0 - 70.0).max(16.0));
                ui.label(RichText::new("No pages").size(20.0).color(p.text));
                ui.add_space(4.0);
                ui.label(RichText::new("Add images, or drop them here").color(p.weak));
                ui.add_space(16.0);
                add = theme::primary_button(ui, "Add images…").clicked();
            });
        });
        if add {
            self.choose_and_add(0);
        }
    }

    /// The bar above the page: its look, and placing its corners.
    fn look_bar(&mut self, ctx: &egui::Context, view: Rect) {
        let Some(page) = self.pages.get(self.selected) else { return };
        let (current, straighten, quad, found, whole) =
            (page.settings.look, page.settings.straighten, page.settings.quad, page.found, page.whole());
        let several = self.pages.len() > 1;
        let mut look = None;
        let mut new_straighten = straighten;
        let handwriting = page.settings.handwriting;
        let mut new_handwriting = handwriting;
        let mut corners = self.corners;
        let mut new_quad = None;
        let mut apply_to: Option<Result<Vec<usize>, String>> = None;
        let count = self.pages.len();
        let response = egui::Area::new(Id::new("image-look"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.center().x, view.top() + 10.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                theme::floating_frame(ui).show(ui, |ui| {
                    ui.set_max_width((view.width() - 32.0).max(200.0));
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for l in Look::ALL {
                            if ui.selectable_label(current == l, l.label()).on_hover_text(l.tip()).clicked() && current != l {
                                look = Some(l);
                            }
                        }
                        if current.scanned() {
                            crate::divider(ui);
                            if icons::button(ui, Icon::Crop, Some("Corners"), corners, true)
                                .on_hover_text("Place the page's corners on the photo")
                                .clicked()
                            {
                                corners = !corners;
                            }
                            ui.checkbox(&mut new_straighten, "Straighten").on_hover_text("Level text that is slightly slanted");
                            if current != Look::Scan(Mode::Original) {
                                ui.checkbox(&mut new_handwriting, "Handwriting").on_hover_text(
                                    "For handwritten pages: darkens faint ink and pencil so every stroke is clear, keeps a notebook's ruled lines light (and leaves them out of black and white). Printed text comes out bolder.",
                                );
                            }
                        }
                        if several {
                            crate::divider(ui);
                            let button = ui.button("Apply to…").on_hover_text("Give other pages this page's look");
                            egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                                ui.set_min_width(220.0);
                                if ui.button("All pages").clicked() {
                                    apply_to = Some(Ok((0..count).collect()));
                                    ui.close();
                                }
                                ui.separator();
                                ui.label(RichText::new("These pages:").size(12.5));
                                ui.horizontal(|ui| {
                                    let field = ui.add(
                                        egui::TextEdit::singleline(&mut self.apply_pages).hint_text("e.g. 1, 3-5").desired_width(110.0),
                                    );
                                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    if ui.button("Apply").clicked() || enter {
                                        apply_to = Some(crate::crop::parse_pages(&self.apply_pages, count));
                                    }
                                });
                                if let Some(e) = &self.apply_error {
                                    ui.colored_label(theme::palette(ui).danger, e);
                                }
                            });
                        }
                    });
                    if corners && current.scanned() {
                        ui.separator();
                        ui.horizontal(|ui| {
                            let p = theme::palette(ui);
                            let hint = if found.is_some() {
                                "Drag the corners onto the page's corners"
                            } else {
                                "No page edges found: drag the corners onto the page's corners"
                            };
                            ui.label(RichText::new(hint).size(12.5).color(p.weak));
                            if quad != whole && ui.button("Whole image").on_hover_text("Use the whole image as the page").clicked() {
                                new_quad = Some(whole);
                            }
                            if let Some(f) = found
                                && quad != f
                                && ui.button("Find page").on_hover_text("Put the corners where the page was found").clicked()
                            {
                                new_quad = Some(f);
                            }
                            if theme::primary_button(ui, "Done").clicked() {
                                corners = false;
                            }
                        });
                    }
                });
            });
        self.bar_height = response.response.rect.height();
        if let Some(l) = look {
            self.set_look(l);
        } else {
            self.corners = corners;
        }
        if new_straighten != straighten {
            self.change(|p| p.settings.straighten = new_straighten);
        }
        if new_handwriting != handwriting {
            self.change(|p| p.settings.handwriting = new_handwriting);
        }
        if let Some(q) = new_quad {
            self.change(|p| p.settings.quad = q);
        }
        match apply_to {
            Some(Ok(positions)) => {
                self.apply_error = None;
                self.apply_look_to(&positions);
                ui_close_popups(ctx);
            }
            Some(Err(e)) => self.apply_error = Some(e),
            None => {}
        }
    }

    /// Page navigation, floating at the bottom.
    fn page_pill(&mut self, ctx: &egui::Context, view: Rect) {
        if view.width() < 220.0 {
            return;
        }
        let count = self.pages.len();
        let mut go = None;
        egui::Area::new(Id::new("image-pill"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.center().x, view.bottom() - 14.0))
            .pivot(egui::Align2::CENTER_BOTTOM)
            .show(ctx, |ui| {
                theme::floating_frame(ui).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if icons::button(ui, Icon::Left, None, false, self.selected > 0).on_hover_text("Previous page (←)").clicked() {
                            go = Some(self.selected - 1);
                        }
                        let p = theme::palette(ui);
                        ui.label(RichText::new(format!("{} / {count}", self.selected + 1)).color(p.text));
                        if icons::button(ui, Icon::Right, None, false, self.selected + 1 < count).on_hover_text("Next page (→)").clicked() {
                            go = Some(self.selected + 1);
                        }
                    });
                });
            });
        if let Some(i) = go {
            self.select(i);
        }
    }

    /// The selected photo, turned as its page will be, with the page's
    /// corners as handles to drag.
    fn corner_editor(&mut self, ui: &mut egui::Ui, area: Rect) {
        let p = theme::palette(ui);
        let page = &self.pages[self.selected];
        let texture = self.large(page.id).map_or(page.thumb.id(), |l| l.texture.id());
        let (size, turns) = (page.size, page.settings.turns);
        let mut quad = page.settings.quad;
        let shown = turned(size, turns);
        let scale = (area.width() / shown.x).min(area.height() / shown.y);
        let image = Rect::from_center_size(area.center(), shown * scale);
        let response = ui.interact(image.expand(30.0), ui.id().with("image-corners"), Sense::click_and_drag());
        let painter = ui.painter().clone();
        paint_turned(&painter, texture, image, full_uv(), turns, Color32::WHITE);
        let screen = |q: [f32; 2]| {
            let t = turn_point(q, size, turns);
            image.min + Vec2::new(t[0], t[1]) * scale
        };
        let corners: Vec<Pos2> = quad.iter().map(|&q| screen(q)).collect();
        let whole = [[0.0, 0.0], [size.x, 0.0], [size.x, size.y], [0.0, size.y]];
        let outer: Vec<Pos2> = whole.iter().map(|&q| screen(q)).collect();

        // Dim the photo outside the page.
        let shade = Color32::from_black_alpha(110);
        for j in 0..4 {
            let mut mesh = egui::Mesh::default();
            for q in [outer[j], outer[(j + 1) % 4], corners[(j + 1) % 4], corners[j]] {
                mesh.colored_vertex(q, shade);
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(Shape::mesh(mesh));
        }
        painter.add(Shape::closed_line(corners.clone(), Stroke::new(2.0, p.accent)));

        // Dragging a corner.
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let near = |pos: Pos2| {
            corners
                .iter()
                .enumerate()
                .map(|(i, c)| (i, c.distance(pos)))
                .filter(|&(_, d)| d < 28.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        if response.drag_started() {
            // Where the press began: by the time a drag is noticed, the
            // pointer may have moved away from the corner.
            let origin = ui.input(|i| i.pointer.press_origin());
            self.drag = origin.or(pointer).and_then(near);
            if self.drag.is_some() {
                self.remember();
            }
        }
        if let (Some(i), Some(pos)) = (self.drag, pointer)
            && response.dragged()
        {
            let d = (pos - image.min) / scale;
            let q = unturn_point([d.x, d.y], size, turns);
            quad[i] = [q[0].clamp(0.0, size.x), q[1].clamp(0.0, size.y)];
            if quad != self.pages[self.selected].settings.quad {
                self.pages[self.selected].settings.quad = quad;
                self.modified = true;
            }
        }
        if response.drag_stopped() {
            self.drag = None;
        }
        let hovered = pointer.and_then(near);
        if self.drag.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if hovered.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        for (i, &q) in quad.iter().enumerate() {
            let r = if self.drag == Some(i) || (self.drag.is_none() && hovered == Some(i)) { 10.0 } else { 8.0 };
            painter.circle(screen(q), r, Color32::WHITE, Stroke::new(2.5, p.accent));
        }

        // A magnifier while dragging, away from the corner.
        if let Some(i) = self.drag {
            let c = screen(quad[i]);
            let half = 60.0;
            let dx = if c.x > area.center().x { -1.0 } else { 1.0 };
            let dy = if c.y > area.center().y { -1.0 } else { 1.0 };
            let lens = Rect::from_center_size(c + Vec2::new(dx, dy) * 95.0, Vec2::splat(2.0 * half));
            // Four times the size it's shown at.
            let zoom = 4.0 * scale;
            let center = Pos2::new(quad[i][0] / size.x, quad[i][1] / size.y);
            let extent = Vec2::new(half / zoom / size.x, half / zoom / size.y);
            let painter = ui.painter().with_clip_rect(lens.expand(4.0));
            painter.rect_filled(lens.expand(3.0), 10.0, p.raised);
            let lens_painter = painter.with_clip_rect(lens);
            // (The lens is square, so turning it needs no other change.)
            paint_turned(&lens_painter, texture, lens, Rect::from_center_size(center, extent * 2.0), turns, Color32::WHITE);
            // The neighboring corners' lines, to line up the edges.
            let to_lens = |q: [f32; 2]| lens.center() + (screen(q) - c) * 4.0;
            for j in [(i + 1) % 4, (i + 3) % 4] {
                lens_painter.line_segment([lens.center(), to_lens(quad[j])], Stroke::new(1.5, p.accent));
            }
            lens_painter.circle_stroke(lens.center(), 5.0, Stroke::new(1.5, p.accent));
            painter.rect_stroke(lens, 8.0, Stroke::new(1.5, p.border), StrokeKind::Outside);
        }
    }
}

/// Closes the open popup menu.
fn ui_close_popups(ctx: &egui::Context) {
    egui::Popup::close_all(ctx);
}

enum SideAction {
    Select(usize),
    Remove(usize),
    /// From a position into the gap before another.
    Move(usize, usize),
    /// Choose images to add at a position.
    Add(usize),
}

/// A page to write: the file, the PDF page (for a PDF's page), and how it
/// looks; with the unsaved edits of the open document for its pages.
type Job = (PathBuf, Option<usize>, Settings, Overlay);

/// Whether a page is a PDF's page kept as it is (copied, not made again).
fn kept(job: &Job) -> bool {
    job.1.is_some() && job.2.look == Look::AsIs
}

/// Adds the page for `job` (not a kept one) to `pdf`.
fn make_page<W: std::io::Write>(pdf: &mut scan::PdfWriter<W>, (path, page, s, overlay): &Job) -> Result<(), String> {
    let name = match page {
        Some(i) => format!("{}, page {}", file_name(path), i + 1),
        None => file_name(path),
    };
    let failed = |e: &dyn std::fmt::Display| format!("{name}: {e}");
    match (s.look, page) {
        (Look::AsIs, _) => {
            let data = std::fs::read(path).map_err(|e| failed(&e))?;
            pdf.add_image(&data, s.turns).map_err(|e| failed(&e))
        }
        (Look::Scan(mode), _) => {
            let image = match page {
                None => Image::open(path).map_err(|e| failed(&e))?,
                Some(_) => read_picture(path, *page, overlay, PDF_PIXELS as u32).map_err(|e| failed(&e))?,
            };
            // The corners are measured on the full-size picture; a PDF
            // page's render may be a pixel off that size.
            let quad = match page {
                None => s.quad,
                Some(i) => {
                    let doc = open_pdf(path, overlay).map_err(|e| failed(&e))?;
                    let pages = doc.pages().map_err(|e| failed(&e))?;
                    let size = pages.get(*i).map_or([image.width, image.height], |p| page_pixels(&doc, p));
                    let k = image.width as f32 / size[0] as f32;
                    s.quad.map(|[x, y]| [x * k, y * k])
                }
            };
            let options =
                ScanOptions { mode, straighten: s.straighten, turns: s.turns, handwriting: s.handwriting, ..ScanOptions::new(quad) };
            pdf.add_page(&scan::scan(&image, &options)).map_err(|e| failed(&e))
        }
    }
}

/// Writes the pages of `jobs` into one PDF at `target`. When some are PDF
/// pages kept as they are, the others are first made into a PDF of their
/// own, then everything is put together from the PDFs (the kept pages
/// turned by setting their rotation, their text and drawings untouched).
fn write_pages(jobs: &[Job], target: &Path, tx: &mpsc::Sender<Msg>) -> Result<(), String> {
    let made = jobs.iter().filter(|j| !kept(j)).count();
    let write_made = |out: &mut dyn std::io::Write| -> Result<(), String> {
        let mut pdf = scan::PdfWriter::new(out).map_err(|e| e.to_string())?;
        for (done, job) in jobs.iter().filter(|j| !kept(j)).enumerate() {
            make_page(&mut pdf, job)?;
            tx.send(Msg::Progress(done + 1)).ok();
        }
        pdf.finish().map_err(|e| e.to_string())?;
        Ok(())
    };
    if made == jobs.len() {
        return crate::write_atomically(target, |out| write_made(out));
    }
    // The made pages, in a file of their own for now.
    static SAVES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SAVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let made_file = std::env::temp_dir().join(format!("k-pdf-made-{}-{n}.pdf", std::process::id()));
    let result = (|| {
        let mut docs: Vec<k_pdf::Document> = Vec::new();
        if made > 0 {
            let mut file = std::io::BufWriter::new(std::fs::File::create(&made_file).map_err(|e| e.to_string())?);
            write_made(&mut file)?;
            std::io::Write::flush(&mut file).map_err(|e| e.to_string())?;
            drop(file);
            docs.push(k_pdf::Document::open(&made_file).map_err(|e| e.to_string())?);
        }
        // Each PDF once per way its pages are turned.
        let mut keys: Vec<(PathBuf, u8)> = Vec::new();
        let mut pages = Vec::with_capacity(jobs.len());
        let mut next_made = 0;
        for job in jobs {
            let (path, page, s, _) = job;
            if !kept(job) {
                pages.push((0, next_made));
                next_made += 1;
                continue;
            }
            let key = (path.clone(), s.turns % 4);
            let d = match keys.iter().position(|k| *k == key) {
                Some(k) => k + usize::from(made > 0),
                None => {
                    let doc = k_pdf::Document::open(path).map_err(|e| format!("{}: {e}", file_name(path)))?;
                    if key.1 != 0 {
                        let mut changes = k_pdf::Changes::default();
                        for i in 0..doc.page_count().map_err(|e| e.to_string())? {
                            // (A page that can't be turned stays as it is.)
                            k_pdf::edit::rotate_page(&doc, i, 90 * key.1 as i32, &mut changes).ok();
                        }
                    }
                    keys.push(key);
                    docs.push(doc);
                    docs.len() - 1
                }
            };
            pages.push((d, page.unwrap_or(0)));
        }
        let refs: Vec<&k_pdf::Document> = docs.iter().collect();
        crate::write_atomically(target, |out| k_pdf::write_document(&refs, &pages, out).map_err(|e| e.to_string()))
    })();
    std::fs::remove_file(&made_file).ok();
    result
}

/// Temporary PDFs of images inserted into an open document: they are read
/// again when saving, so they're kept until k-pdf closes.
#[derive(Default)]
pub struct TempFiles(Vec<PathBuf>);

impl TempFiles {
    pub fn contains(&self, path: &Path) -> bool {
        self.0.iter().any(|p| p == path)
    }

    pub fn next(&mut self) -> PathBuf {
        self.next_named("images")
    }

    /// A temporary PDF for `what` ("images", "form"…).
    pub fn next_named(&mut self, what: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("k-pdf-{what}-{}-{}.pdf", std::process::id(), self.0.len() + 1));
        self.0.push(path.clone());
        path
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            std::fs::remove_file(path).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_recognized() {
        assert!(is_image(Path::new("a/b/IMG_0001.JPG")));
        assert!(is_image(Path::new("scan.png")));
        assert!(is_image(Path::new("fax.TIFF")));
        assert!(is_image(Path::new("photo.webp")));
        assert!(!is_image(Path::new("doc.pdf")));
        assert!(!is_image(Path::new("noext")));
    }

    #[test]
    fn turning_points_and_back() {
        let size = Vec2::new(40.0, 30.0);
        // A quarter turn clockwise: the top left corner goes to the top right.
        assert_eq!(turn_point([0.0, 0.0], size, 1), [30.0, 0.0]);
        assert_eq!(turn_point([40.0, 0.0], size, 1), [30.0, 40.0]);
        for turns in 0..4 {
            for p in [[0.0, 0.0], [12.0, 5.0], [40.0, 30.0], [3.0, 29.0]] {
                assert_eq!(unturn_point(turn_point(p, size, turns), size, turns), p, "turns {turns}");
            }
        }
    }
}
