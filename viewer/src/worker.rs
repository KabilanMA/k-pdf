//! Background rendering: a small pool of threads sharing one job queue.
//!
//! `Document` is deliberately not thread-safe (it uses interior caches), so
//! each thread opens the file itself. The file is memory-mapped, so the OS
//! shares its pages between threads and the extra cost is small.
//!
//! The UI replaces the queue whenever the set of needed pages changes, so
//! fast scrolling or zooming never builds up a backlog.
//!
//! Besides the main document, other PDFs can be opened alongside it (pages
//! inserted from other files). Pages are then identified by a *key*: the
//! main document's pages come first (keys 0..n), then each added
//! document's pages in turn.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use eframe::egui;
use k_pdf::search::{Match, Query};
use k_pdf::{Document, Link, Renderer, TextChar, TextExtractor};

/// Largest texture side we ask for; beyond this pages are upscaled.
pub const MAX_TEXTURE_SIDE: f32 = 8192.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Job {
    Render { page: usize, scale: f32 },
    Chars { page: usize },
    /// A small rendering for the page sidebar.
    Thumb { page: usize, scale: f32 },
    /// Searching a page's text for the current query (`id`).
    Search { page: usize, id: u64 },
    /// Reading the text in a page's images (slow: one at a time).
    Ocr { page: usize },
}

impl Job {
    /// Jobs run in this order: what's on screen first, reading images last.
    fn rank(&self) -> u8 {
        match self {
            Job::Render { .. } => 0,
            Job::Chars { .. } => 1,
            Job::Thumb { .. } => 2,
            Job::Search { .. } => 3,
            Job::Ocr { .. } => 4,
        }
    }
}

fn sort_jobs(jobs: &mut [Job]) {
    jobs.sort_by_key(Job::rank);
}

pub struct DocInfo {
    pub path: PathBuf,
    pub version: String,
    /// Size of the file in bytes (as opened).
    pub file_size: u64,
    /// Displayed page sizes in points (after rotation).
    pub page_sizes: Vec<egui::Vec2>,
    pub metadata: Vec<(&'static str, String)>,
    pub encrypted: bool,
    pub repaired: bool,
    /// Form fields, filled in by the viewer.
    pub fields: Vec<k_pdf::FormField>,
    /// Sections and subsections (the file's bookmarks), for the sidebar.
    pub outline: Vec<k_pdf::OutlineItem>,
}

pub enum Reply {
    Opened(Box<DocInfo>),
    OpenFailed(PathBuf, String),
    /// `dark`: rendered as a dark page (see [`Pool::set_dark_pages`]).
    Page { page: usize, scale: f32, dark: bool, image: egui::ColorImage },
    PageFailed { page: usize, error: String },
    /// A page's text (in reading order) and links.
    Chars { page: usize, chars: Arc<Vec<TextChar>>, links: Arc<Vec<Link>> },
    Thumb { page: usize, dark: bool, image: egui::ColorImage },
    /// Where the search `id` found its phrase on a page, and whether the
    /// page has images (whose text can be read with [`Pool::want_ocr`]).
    Found { page: usize, id: u64, matches: Vec<Match>, images: bool },
    /// The text read in a page's images (empty if it has none).
    Ocr { page: usize, chars: Arc<Vec<TextChar>> },
    OcrFailed { page: usize, error: String },
    /// A document added with [`Pool::add_document`] is open; its pages have
    /// keys `first..first + page_sizes.len()`.
    Added { path: PathBuf, first: usize, page_sizes: Vec<egui::Vec2> },
    AddFailed(PathBuf, String),
}

struct State {
    /// Incremented on every open; replies from older documents are dropped.
    doc_id: u64,
    path: Option<PathBuf>,
    /// Documents opened alongside the main one, in the order added.
    added: Vec<PathBuf>,
    /// Unsaved edits of the main document (see `Document::set_overlay`),
    /// and a counter that changes whenever they do.
    overlay: Option<Arc<k_pdf::Changes>>,
    overlay_version: u64,
    /// Render pages dark for reading.
    dark: bool,
    /// The phrase being searched for, and its search's id.
    search: Option<(u64, Arc<Query>)>,
    /// Pending jobs, most important first.
    jobs: Vec<Job>,
    /// Jobs a thread is working on right now, and whether it renders them
    /// dark: a job started before dark pages were switched doesn't count
    /// as the one wanted now.
    running: Vec<(Job, bool)>,
    closed: bool,
}

impl State {
    /// Whether `job` is being worked on as currently wanted.
    fn is_running(&self, job: &Job) -> bool {
        self.running.contains(&(*job, self.dark))
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// Handle owned by the UI.
pub struct Pool {
    shared: Arc<Shared>,
    replies: Receiver<(u64, Reply)>,
}

impl Pool {
    pub fn start(ctx: egui::Context) -> Pool {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                doc_id: 0,
                path: None,
                added: Vec::new(),
                overlay: None,
                overlay_version: 0,
                dark: false,
                search: None,
                jobs: Vec::new(),
                running: Vec::new(),
                closed: false,
            }),
            wake: Condvar::new(),
        });
        let (tx, rx) = channel();
        // A few threads speed up first display and fast scrolling; more
        // would mostly cost memory (each keeps its own font caches).
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 3);
        for index in 0..threads {
            let (shared, tx, ctx) = (shared.clone(), tx.clone(), ctx.clone());
            std::thread::Builder::new()
                .name(format!("pdf-render-{index}"))
                .spawn(move || worker(index, &shared, &tx, &ctx))
                .expect("failed to start render thread");
        }
        Pool { shared, replies: rx }
    }

    pub fn open(&self, path: PathBuf) {
        let mut st = self.shared.state.lock().unwrap();
        st.doc_id += 1;
        st.path = Some(path);
        st.added.clear();
        st.overlay = None;
        st.overlay_version += 1;
        st.search = None;
        st.jobs.clear();
        self.shared.wake.notify_all();
    }

    /// Replaces the queued page renders. Other queued jobs are kept, after
    /// them (the main view comes first).
    pub fn want_pages(&self, pages: &[usize], scale: f32) {
        let mut st = self.shared.state.lock().unwrap();
        let mut jobs: Vec<Job> = pages
            .iter()
            .map(|&page| Job::Render { page, scale })
            .filter(|j| !st.is_running(j))
            .collect();
        jobs.extend(st.jobs.iter().filter(|j| !matches!(j, Job::Render { .. })));
        sort_jobs(&mut jobs);
        st.jobs = jobs;
        self.shared.wake.notify_all();
    }

    /// Searches `pages` (keys, in the order given) for `query`, answered
    /// with [`Reply::Found`] tagged `id`. `None` stops searching.
    pub fn search(&self, id: u64, query: Option<Query>, pages: &[usize]) {
        let mut st = self.shared.state.lock().unwrap();
        st.jobs.retain(|j| !matches!(j, Job::Search { .. }));
        st.search = query.map(|q| (id, Arc::new(q)));
        if st.search.is_some() {
            st.jobs.extend(pages.iter().map(|&page| Job::Search { page, id }));
        }
        sort_jobs(&mut st.jobs);
        self.shared.wake.notify_all();
    }

    /// Reads the text in a page's images ([`Reply::Ocr`]). Pages are read
    /// one at a time, after everything else.
    pub fn want_ocr(&self, page: usize) {
        let mut st = self.shared.state.lock().unwrap();
        let job = Job::Ocr { page };
        if !st.jobs.contains(&job) && !st.running.iter().any(|(j, _)| *j == job) {
            st.jobs.push(job);
            sort_jobs(&mut st.jobs);
            self.shared.wake.notify_all();
        }
    }

    /// Drops queued image reading, except for `keep`.
    pub fn cancel_ocr(&self, keep: &std::collections::HashSet<usize>) {
        let mut st = self.shared.state.lock().unwrap();
        st.jobs.retain(|j| !matches!(j, Job::Ocr { page } if !keep.contains(page)));
    }

    /// Replaces the queued thumbnail renders (`(page, scale)` pairs). They
    /// run after everything else.
    pub fn want_thumbs(&self, thumbs: &[(usize, f32)]) {
        let mut st = self.shared.state.lock().unwrap();
        st.jobs.retain(|j| !matches!(j, Job::Thumb { .. }));
        let new: Vec<Job> = thumbs
            .iter()
            .map(|&(page, scale)| Job::Thumb { page, scale })
            .filter(|j| !st.is_running(j))
            .collect();
        st.jobs.extend(new);
        sort_jobs(&mut st.jobs);
        self.shared.wake.notify_all();
    }

    /// Shows the main document with `changes` applied (unsaved edits).
    /// Pages already rendered must be requested again.
    pub fn set_overlay(&self, changes: Arc<k_pdf::Changes>) {
        let mut st = self.shared.state.lock().unwrap();
        st.overlay = Some(changes);
        st.overlay_version += 1;
        st.jobs.clear();
        self.shared.wake.notify_all();
    }

    /// Renders pages dark for reading (or normally again). Pages already
    /// rendered must be requested again; replies say which way they were
    /// rendered, so late ones can be dropped.
    pub fn set_dark_pages(&self, dark: bool) {
        let mut st = self.shared.state.lock().unwrap();
        if st.dark != dark {
            st.dark = dark;
            // Text, searching and reading images don't depend on it.
            st.jobs.retain(|j| matches!(j, Job::Chars { .. } | Job::Search { .. } | Job::Ocr { .. }));
        }
        self.shared.wake.notify_all();
    }

    /// Opens another document next to the main one (to insert its pages).
    /// Answered with [`Reply::Added`] or [`Reply::AddFailed`].
    pub fn add_document(&self, path: PathBuf) {
        let mut st = self.shared.state.lock().unwrap();
        st.added.push(path);
        self.shared.wake.notify_all();
    }

    pub fn want_chars(&self, page: usize) {
        let mut st = self.shared.state.lock().unwrap();
        let job = Job::Chars { page };
        if !st.jobs.contains(&job) && !st.is_running(&job) {
            st.jobs.push(job);
            self.shared.wake.notify_all();
        }
    }

    /// Replies for the current document only.
    pub fn poll(&self) -> Vec<Reply> {
        let current = self.shared.state.lock().unwrap().doc_id;
        self.replies.try_iter().filter(|(id, _)| *id == current).map(|(_, r)| r).collect()
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().closed = true;
        self.shared.wake.notify_all();
    }
}

/// An open document with its pages; `None` if it couldn't be opened.
type Opened = Option<(Document, Vec<k_pdf::Page>)>;

fn open_document(path: &PathBuf) -> k_pdf::Result<(Document, Vec<k_pdf::Page>)> {
    let doc = Document::open(path)?;
    let pages = doc.pages()?;
    Ok((doc, pages))
}

fn worker(index: usize, shared: &Shared, replies: &Sender<(u64, Reply)>, ctx: &egui::Context) {
    let send = |id: u64, r: Reply| {
        let _ = replies.send((id, r));
        ctx.request_repaint();
    };
    let mut my_id = 0;

    'documents: loop {
        // Wait until there is a (new) document to open.
        let (id, path) = {
            let mut st = shared.state.lock().unwrap();
            while !st.closed && (st.path.is_none() || st.doc_id == my_id) {
                st = shared.wake.wait(st).unwrap();
            }
            if st.closed {
                return;
            }
            (st.doc_id, st.path.clone().unwrap_or_default())
        };
        my_id = id;
        let main = match open_document(&path) {
            Ok(v) => v,
            Err(e) => {
                if index == 0 {
                    send(id, Reply::OpenFailed(path, e.to_string()));
                }
                continue;
            }
        };
        // Thread 0 announces documents; the others just get ready.
        if index == 0 {
            send(id, Reply::Opened(Box::new(doc_info(&main.0, &main.1, path))));
        }
        let mut docs: Vec<Opened> = vec![Some(main)];
        let mut overlay_version = u64::MAX;

        loop {
            // Apply edits to the main document.
            let (version, overlay) = {
                let st = shared.state.lock().unwrap();
                (st.overlay_version, st.overlay.clone())
            };
            if version != overlay_version {
                overlay_version = version;
                if let Some((doc, pages)) = &mut docs[0] {
                    doc.set_overlay(overlay.as_deref().unwrap_or(&k_pdf::Changes::default()));
                    if let Ok(p) = doc.pages() {
                        *pages = p;
                    }
                }
            }
            // Open documents added since last time.
            let added: Vec<PathBuf> = shared.state.lock().unwrap().added.iter().skip(docs.len() - 1).cloned().collect();
            for path in added {
                let first = page_total(&docs);
                let opened = open_document(&path).and_then(|d| {
                    if d.0.is_encrypted() { Err(k_pdf::Error::Encrypted) } else { Ok(d) }
                });
                match opened {
                    Ok((doc, pages)) => {
                        if index == 0 {
                            let page_sizes = page_sizes(&doc, &pages);
                            send(id, Reply::Added { path, first, page_sizes });
                        }
                        docs.push(Some((doc, pages)));
                    }
                    Err(e) => {
                        if index == 0 {
                            send(id, Reply::AddFailed(path, e.to_string()));
                        }
                        docs.push(None);
                    }
                }
            }

            // Renderers and text extractors borrow the documents, so they
            // are rebuilt (losing their font caches) when one is added.
            let mut tools: Vec<Option<(Renderer, TextExtractor)>> =
                docs.iter().map(|d| d.as_ref().map(|(doc, _)| (Renderer::new(doc), TextExtractor::new(doc)))).collect();
            let starts = page_starts(&docs);
            loop {
                let job = {
                    let mut st = shared.state.lock().unwrap();
                    while !st.closed
                        && st.doc_id == id
                        && next_job(&st).is_none()
                        && st.added.len() + 1 == docs.len()
                        && st.overlay_version == overlay_version
                    {
                        st = shared.wake.wait(st).unwrap();
                    }
                    if st.closed {
                        return;
                    }
                    if st.doc_id != id {
                        continue 'documents; // a new document was opened
                    }
                    if st.added.len() + 1 != docs.len() || st.overlay_version != overlay_version {
                        break; // another document was added, or edits changed
                    }
                    let Some(i) = next_job(&st) else { continue };
                    let job = st.jobs.remove(i);
                    // A search that was replaced meanwhile.
                    if let Job::Search { id: wanted, .. } = job
                        && st.search.as_ref().is_none_or(|(current, _)| *current != wanted)
                    {
                        continue;
                    }
                    let dark = st.dark;
                    st.running.push((job, dark));
                    (job, dark, st.search.as_ref().map(|(_, q)| q.clone()))
                };
                let (job, dark, query) = job;
                // Which document the page belongs to.
                let key = job.page();
                let d = starts.partition_point(|&s| s <= key).saturating_sub(1);
                let reply = match (&docs[d], &mut tools[d]) {
                    (Some((doc, pages)), Some((renderer, extractor))) => {
                        // Thumbnails keep the fields as saved in the file.
                        // The main document's form fields are drawn by the
                        // viewer as editable widgets (made later too, in
                        // form mode), so its page renders leave their
                        // contents out.
                        renderer.set_hide_form_fields(d == 0 && matches!(job, Job::Render { .. }));
                        renderer.set_dark_pages(dark);
                        run_job(job.with_page(key - starts[d]), doc, pages, renderer, extractor, dark, query.as_deref())
                            .map(|r| r.with_page_offset(starts[d]))
                    }
                    _ => None,
                };
                {
                    let mut st = shared.state.lock().unwrap();
                    if let Some(i) = st.running.iter().position(|r| *r == (job, dark)) {
                        st.running.remove(i);
                    }
                    // Another thread may be waiting to read the next page's images.
                    if matches!(job, Job::Ocr { .. }) {
                        shared.wake.notify_all();
                    }
                }
                if let Some(r) = reply {
                    send(id, r);
                }
            }
        }
    }
}

/// The first queued job that may run now: reading images only when no
/// other thread is (it uses several threads itself, and a lot of memory).
fn next_job(st: &State) -> Option<usize> {
    let reading = st.running.iter().any(|(j, _)| matches!(j, Job::Ocr { .. }));
    st.jobs.iter().position(|j| !(reading && matches!(j, Job::Ocr { .. })))
}

/// The text-recognition engine, loaded the first time it's needed.
fn ocr_engine() -> Result<Arc<k_pdf::ocr::Ocr>, String> {
    static ENGINE: OnceLock<Result<Arc<k_pdf::ocr::Ocr>, String>> = OnceLock::new();
    ENGINE.get_or_init(|| k_pdf::ocr::Ocr::find().map(Arc::new).map_err(|e| e.to_string())).clone()
}

/// Whether text in images can be read (the models are installed).
pub fn ocr_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| k_pdf::ocr::find_models().is_some())
}

fn page_total(docs: &[Opened]) -> usize {
    docs.iter().map(|d| d.as_ref().map_or(0, |(_, p)| p.len())).sum()
}

/// First page key of each document.
fn page_starts(docs: &[Opened]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(docs.len());
    let mut total = 0;
    for d in docs {
        starts.push(total);
        total += d.as_ref().map_or(0, |(_, p)| p.len());
    }
    starts
}

impl Job {
    fn page(&self) -> usize {
        match *self {
            Job::Render { page, .. }
            | Job::Chars { page }
            | Job::Thumb { page, .. }
            | Job::Search { page, .. }
            | Job::Ocr { page } => page,
        }
    }

    fn with_page(mut self, p: usize) -> Job {
        match &mut self {
            Job::Render { page, .. }
            | Job::Chars { page }
            | Job::Thumb { page, .. }
            | Job::Search { page, .. }
            | Job::Ocr { page } => *page = p,
        }
        self
    }
}

impl Reply {
    /// Turns page numbers within one document into page keys.
    fn with_page_offset(mut self, offset: usize) -> Reply {
        match &mut self {
            Reply::Page { page, .. }
            | Reply::PageFailed { page, .. }
            | Reply::Thumb { page, .. }
            | Reply::Found { page, .. }
            | Reply::Ocr { page, .. }
            | Reply::OcrFailed { page, .. } => *page += offset,
            Reply::Chars { page, links, .. } => {
                *page += offset;
                if offset > 0 {
                    let mut moved = links.as_ref().clone();
                    for link in &mut moved {
                        if let k_pdf::LinkTarget::Page { page, .. } = &mut link.target {
                            *page += offset;
                        }
                    }
                    *links = Arc::new(moved);
                }
            }
            _ => {}
        }
        self
    }
}

fn page_sizes(doc: &Document, pages: &[k_pdf::Page]) -> Vec<egui::Vec2> {
    pages
        .iter()
        .map(|p| {
            let (w, h) = p.display_size(doc);
            egui::vec2(w as f32, h as f32)
        })
        .collect()
}

fn run_job(
    job: Job,
    doc: &Document,
    pages: &[k_pdf::Page],
    renderer: &mut Renderer,
    extractor: &mut TextExtractor,
    dark: bool,
    query: Option<&Query>,
) -> Option<Reply> {
    // A bug on one page must not take down the thread.
    let guard = |f: &mut dyn FnMut() -> Reply| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
    };
    match job {
        Job::Search { page, id } => {
            let (p, query) = (pages.get(page)?, query?);
            guard(&mut || {
                let chars = extractor.extract_chars(p).unwrap_or_default();
                let matches = query.find(&k_pdf::reading_order(&chars));
                Reply::Found { page, id, matches, images: k_pdf::ocr::page_has_images(doc, p) }
            })
        }
        Job::Ocr { page } => {
            let p = pages.get(page)?;
            let engine = match ocr_engine() {
                Ok(engine) => engine,
                Err(error) => return Some(Reply::OcrFailed { page, error }),
            };
            let reply = guard(&mut || {
                // Read from the page as printed, not as shown dark.
                renderer.set_dark_pages(false);
                let chars = extractor.extract_chars(p).unwrap_or_default();
                match k_pdf::ocr::read_page_images(&engine, doc, renderer, p, &chars) {
                    Ok(read) => Reply::Ocr { page, chars: Arc::new(read) },
                    Err(e) => Reply::OcrFailed { page, error: e.to_string() },
                }
            });
            k_pdf::ocr::release_memory();
            Some(reply.unwrap_or(Reply::OcrFailed { page, error: "something went wrong".into() }))
        }
        Job::Chars { page } => guard(&mut || {
            let chars = pages.get(page).and_then(|p| extractor.extract_chars(p).ok()).unwrap_or_default();
            // Reading order keeps selections inside a column (not across margin
            // line numbers or the neighbouring column).
            let chars = k_pdf::reading_order(&chars);
            let links = k_pdf::page_links(doc, pages, page);
            Reply::Chars { page, chars: Arc::new(chars), links: Arc::new(links) }
        }),
        Job::Thumb { page, scale } => {
            let p = pages.get(page)?;
            guard(&mut || match renderer.render_page(p, scale as f64) {
                Ok(img) => Reply::Thumb {
                    page,
                    dark,
                    image: egui::ColorImage::from_rgba_premultiplied([img.width as usize, img.height as usize], &img.pixels),
                },
                // The sidebar just keeps showing a blank page.
                Err(_) => Reply::Thumb { page, dark, image: egui::ColorImage::filled([1, 1], crate::theme::paper(dark)) },
            })
        }
        Job::Render { page: p, scale } => {
            let page = pages.get(p)?;
            // Keep the bitmap within texture limits.
            let (w, h) = page.display_size(doc);
            let s = scale.min(MAX_TEXTURE_SIDE / w.max(h).max(1.0) as f32);
            guard(&mut || match renderer.render_page(page, s as f64) {
                Ok(img) => Reply::Page {
                    page: p,
                    scale,
                    dark,
                    image: egui::ColorImage::from_rgba_premultiplied([img.width as usize, img.height as usize], &img.pixels),
                },
                Err(e) => Reply::PageFailed { page: p, error: e.to_string() },
            })
            .or(Some(Reply::PageFailed { page: p, error: "internal error while rendering this page".into() }))
        }
    }
}

fn doc_info(doc: &Document, pages: &[k_pdf::Page], path: PathBuf) -> DocInfo {
    DocInfo {
        file_size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        path,
        version: doc.version(),
        page_sizes: page_sizes(doc, pages),
        metadata: metadata(doc),
        encrypted: doc.is_encrypted(),
        repaired: doc.was_repaired(),
        fields: k_pdf::form_fields(doc).unwrap_or_default(),
        outline: k_pdf::outline(doc, pages),
    }
}

fn metadata(doc: &Document) -> Vec<(&'static str, String)> {
    let Ok(m) = doc.metadata() else { return Vec::new() };
    [
        ("Title", m.title),
        ("Author", m.author),
        ("Subject", m.subject),
        ("Keywords", m.keywords),
        ("Creator", m.creator),
        ("Producer", m.producer),
        ("Created", m.creation_date.map(|d| pretty_date(&d))),
        ("Modified", m.mod_date.map(|d| pretty_date(&d))),
    ]
    .into_iter()
    .filter_map(|(k, v)| {
        let v: String = v?.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let v = v.trim().to_string();
        (!v.is_empty()).then_some((k, v))
    })
    .collect()
}

/// `D:20240131120000+01'00'` → `2024-01-31 12:00:00 +01:00`.
pub fn pretty_date(raw: &str) -> String {
    let s = raw.strip_prefix("D:").unwrap_or(raw);
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 8 {
        return raw.to_string();
    }
    let part = |a: usize, b: usize| digits.get(a..b).unwrap_or("00");
    let mut out = format!("{}-{}-{}", part(0, 4), part(4, 6), part(6, 8));
    if digits.len() >= 12 {
        out += &format!(" {}:{}:{}", part(8, 10), part(10, 12), part(12, 14));
    }
    let tz = &s[digits.len()..];
    if tz.starts_with('Z') {
        out += " UTC";
    } else if let Some(sign) = tz.chars().next().filter(|c| *c == '+' || *c == '-') {
        let tzd: String = tz.chars().filter(char::is_ascii_digit).collect();
        if tzd.len() >= 4 {
            out += &format!(" {sign}{}:{}", &tzd[..2], &tzd[2..4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::pretty_date("D:20240131120000+01'00'"), "2024-01-31 12:00:00 +01:00");
        assert_eq!(super::pretty_date("D:19990412"), "1999-04-12");
        assert_eq!(super::pretty_date("garbage"), "garbage");
    }
}
