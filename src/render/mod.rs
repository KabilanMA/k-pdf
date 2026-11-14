//! Page rendering: interprets content streams and rasterizes them with
//! tiny-skia (ISO 32000 §8–9, §12.5.5 for annotation appearances).
//!
//! Supported: paths, fills/strokes, clipping, device/ICC/indexed/
//! separation/DeviceN/Lab colors, axial/radial shadings, tiling and shading
//! patterns, images (raw, JPEG, masks, soft masks), embedded TrueType/
//! OpenType/CFF/Type 1/Type 3 fonts with system-font fallback, form
//! XObjects, annotation appearances, optional content (hidden layers) and
//! blend modes. Not yet: soft-mask groups, JPEG 2000/JBIG2/CCITT images.

mod color;
pub mod dark;
mod function;
mod glyphs;
pub(crate) mod image;
mod shading;
mod type1;

use crate::hash::{HashMap, HashSet};
use std::rc::Rc;

use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, LineCap, LineJoin, Mask, Paint, Path, PathBuilder, Pattern, Pixmap,
    PixmapPaint, Rect, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use self::color::ColorSpace;
use self::glyphs::{RenderFont, SystemFonts};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::parser::{Parser, Token};

type Matrix = [f64; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
/// Nesting limit for forms, patterns and Type 3 glyphs.
const MAX_DEPTH: usize = 16;
/// Largest page bitmap we will allocate (pixels).
const MAX_PAGE_PIXELS: u64 = 120_000_000;
/// Largest tiling pattern cell (pixels per side).
const MAX_TILE: f64 = 2048.0;

/// A rendered page: `width × height` opaque RGBA pixels, row by row.
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// Where images were drawn: `[left, top, right, bottom]` in pixels,
    /// within the page (for reading text in images, see `ocr`). Images
    /// that couldn't be decoded are left out.
    pub images: Vec<[f32; 4]>,
}

/// Renders pages of one document. Keeps fonts loaded between pages, so
/// reuse one `Renderer` for the whole document.
pub struct Renderer<'d> {
    doc: &'d Document,
    fonts: HashMap<ObjRef, Rc<RenderFont>>,
    system_fonts: SystemFonts,
    hidden_layers: HashSet<ObjRef>,
    hide_form_fields: bool,
    /// Dark pages for reading (see [`dark`]), and the photos (device pixel
    /// rectangles) to leave as they are on the page being rendered.
    dark_pages: bool,
    photos: Vec<dark::Keep>,
    /// Images drawn on the page being rendered (see `RenderedPage::images`).
    images: Vec<[f32; 4]>,
}

impl<'d> Renderer<'d> {
    pub fn new(doc: &'d Document) -> Self {
        Renderer {
            doc,
            fonts: HashMap::default(),
            system_fonts: SystemFonts::default(),
            hidden_layers: hidden_layers(doc),
            hide_form_fields: false,
            dark_pages: false,
            photos: Vec::new(),
            images: Vec::new(),
        }
    }

    /// Leaves out the contents of fillable form fields (text, choice, check
    /// box and radio button widgets), drawing only their background and
    /// border, for viewers that draw editable fields on top of the page.
    pub fn set_hide_form_fields(&mut self, hide: bool) {
        self.hide_form_fields = hide;
    }

    /// Renders pages dark for reading: lightness turned around, hues kept,
    /// photos left as they are (see [`dark`]).
    pub fn set_dark_pages(&mut self, dark: bool) {
        self.dark_pages = dark;
    }

    /// Renders `page` at `scale` pixels per point (1.0 = 72 dpi).
    pub fn render_page(&mut self, page: &Page, scale: f64) -> Result<RenderedPage> {
        let doc = self.doc;
        let s = scale;
        // Maps page space to device pixels, including the /Rotate flip.
        let base: Matrix = mul(&page.display_matrix(doc), &[s, 0.0, 0.0, s, 0.0, 0.0]);
        let (w, h) = page.display_size(doc);
        let (pw, ph) = ((w * s).round().max(1.0) as u32, (h * s).round().max(1.0) as u32);
        if pw as u64 * ph as u64 > MAX_PAGE_PIXELS {
            return Err(Error::Object(format!("page is too large to render at this scale ({pw}×{ph})")));
        }
        let mut canvas = Pixmap::new(pw, ph).ok_or_else(|| Error::Object("cannot allocate page bitmap".into()))?;
        canvas.fill(Color::WHITE);

        let content = doc.page_content(page)?;
        let resources = doc.get_dict(&page.dict, b"Resources").unwrap_or_default();
        self.run(&mut canvas, &content, &resources, GState::new(base), base, 0);
        self.draw_annotations(&mut canvas, page, &resources, base);

        // The page is opaque, so premultiplied and straight RGBA coincide.
        let mut pixels = canvas.take();
        if self.dark_pages {
            dark::darken(&mut pixels, pw, &self.photos);
        }
        self.photos.clear();
        let images = std::mem::take(&mut self.images);
        Ok(RenderedPage { width: pw, height: ph, pixels, images })
    }

    // ------------------------------------------------------------------
    // Content stream interpreter
    // ------------------------------------------------------------------

    /// Runs a content stream. `base` is the space patterns are defined in.
    fn run(&mut self, canvas: &mut Pixmap, content: &[u8], resources: &Dictionary, mut gs: GState, base: Matrix, depth: usize) {
        let doc = self.doc;
        let mut stack: Vec<GState> = Vec::new();
        let mut path = PathBuilder::new();
        let mut current = (0.0f32, 0.0f32);
        let mut pending_clip: Option<FillRule> = None;
        let mut tm = IDENTITY;
        let mut tlm = IDENTITY;
        let mut text_clip: Option<PathBuilder> = None;
        // Marked-content nesting; `true` entries hide content (layers).
        let mut hidden: Vec<bool> = Vec::new();
        let mut operands: Vec<Object> = Vec::new();
        let mut parser = Parser::new(content, 0);

        while let Some(tok) = parser.lex.next_token() {
            let op = match tok {
                Token::Keyword(op) => op,
                other => {
                    match parser.object_from_token(other, 0) {
                        Ok(o) => operands.push(o),
                        Err(_) => operands.clear(),
                    }
                    continue;
                }
            };
            let num = |i: usize| operands.get(i).and_then(Object::as_f64).unwrap_or(0.0);
            let f = |i: usize| num(i) as f32;
            let invisible = hidden.contains(&true);

            match op {
                // --- graphics state ---
                b"q" => stack.push(gs.clone()),
                b"Q" => {
                    if let Some(saved) = stack.pop() {
                        gs = saved;
                    }
                }
                b"cm" if operands.len() >= 6 => {
                    gs.ctm = mul(&[num(0), num(1), num(2), num(3), num(4), num(5)], &gs.ctm);
                }
                b"w" => gs.line_width = num(0),
                b"J" => {
                    gs.cap = match num(0) as i64 {
                        1 => LineCap::Round,
                        2 => LineCap::Square,
                        _ => LineCap::Butt,
                    }
                }
                b"j" => {
                    gs.join = match num(0) as i64 {
                        1 => LineJoin::Round,
                        2 => LineJoin::Bevel,
                        _ => LineJoin::Miter,
                    }
                }
                b"M" => gs.miter = num(0),
                b"d" => {
                    if let Some(Object::Array(a)) = operands.first() {
                        gs.dash = make_dash(a, num(1));
                    }
                }
                b"gs" => {
                    if let Some(name) = operands.first().and_then(Object::as_name) {
                        self.apply_ext_gstate(canvas, &mut gs, resources, name, depth);
                    }
                }

                // --- path construction ---
                b"m" => {
                    current = (f(0), f(1));
                    path.move_to(current.0, current.1);
                }
                b"l" => {
                    current = (f(0), f(1));
                    path.line_to(current.0, current.1);
                }
                b"c" => {
                    path.cubic_to(f(0), f(1), f(2), f(3), f(4), f(5));
                    current = (f(4), f(5));
                }
                b"v" => {
                    path.cubic_to(current.0, current.1, f(0), f(1), f(2), f(3));
                    current = (f(2), f(3));
                }
                b"y" => {
                    path.cubic_to(f(0), f(1), f(2), f(3), f(2), f(3));
                    current = (f(2), f(3));
                }
                b"h" => path.close(),
                b"re" => {
                    let (x, y, w, h) = (f(0), f(1), f(2), f(3));
                    path.move_to(x, y);
                    path.line_to(x + w, y);
                    path.line_to(x + w, y + h);
                    path.line_to(x, y + h);
                    path.close();
                    current = (x, y);
                }

                // --- path painting ---
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
                    if matches!(op, b"s" | b"b" | b"b*") {
                        path.close();
                    }
                    let built = std::mem::replace(&mut path, PathBuilder::new()).finish();
                    if let Some(p) = &built
                        && !invisible
                    {
                        let fill = match op {
                            b"f" | b"F" | b"B" | b"b" => Some(FillRule::Winding),
                            b"f*" | b"B*" | b"b*" => Some(FillRule::EvenOdd),
                            _ => None,
                        };
                        let stroke = matches!(op, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*");
                        if let Some(rule) = fill {
                            self.fill_path(canvas, &gs, p, rule, base, depth);
                        }
                        if stroke {
                            self.stroke_path(canvas, &gs, p, base, depth);
                        }
                    }
                    if let Some(rule) = pending_clip.take() {
                        let ts = to_ts(&gs.ctm);
                        intersect_clip(canvas, &mut gs, built.as_ref(), rule, ts);
                    }
                }
                b"W" => pending_clip = Some(FillRule::Winding),
                b"W*" => pending_clip = Some(FillRule::EvenOdd),

                // --- color ---
                b"CS" | b"cs" => {
                    if let Some(obj) = operands.first()
                        && let Some(cs) = ColorSpace::parse(doc, obj, resources)
                    {
                        let ink = if op == b"CS" { &mut gs.stroke } else { &mut gs.fill };
                        ink.set_space(cs);
                    }
                }
                b"SC" | b"SCN" | b"sc" | b"scn" => {
                    let ink = if op.starts_with(b"S") { &mut gs.stroke } else { &mut gs.fill };
                    let comps: Vec<f64> = operands.iter().filter_map(Object::as_f64).collect();
                    if let Some(name) = operands.last().and_then(Object::as_name) {
                        ink.pattern = doc.get_dict(resources, b"Pattern").and_then(|p| p.get(name).cloned());
                    }
                    ink.set_components(comps);
                }
                b"G" | b"g" | b"RG" | b"rg" | b"K" | b"k" => {
                    let cs = match op {
                        b"G" | b"g" => ColorSpace::Gray,
                        b"RG" | b"rg" => ColorSpace::Rgb,
                        _ => ColorSpace::Cmyk,
                    };
                    let comps: Vec<f64> = operands.iter().filter_map(Object::as_f64).collect();
                    let ink = if op[0].is_ascii_uppercase() { &mut gs.stroke } else { &mut gs.fill };
                    ink.set_space(cs);
                    ink.set_components(comps);
                }
                b"sh" if !invisible => {
                    if let Some(name) = operands.first().and_then(Object::as_name) {
                        self.paint_shading(canvas, &gs, resources, name);
                    }
                }

                // --- external objects ---
                b"Do" if !invisible && depth < MAX_DEPTH => {
                    if let Some(name) = operands.first().and_then(Object::as_name) {
                        self.do_xobject(canvas, &gs, resources, name, depth);
                    }
                }
                b"BI" => {
                    if let Some(img) = read_inline_image(&mut parser)
                        && !invisible
                    {
                        self.draw_image(canvas, &gs, &img, resources);
                    }
                }

                // --- text ---
                b"BT" => {
                    tm = IDENTITY;
                    tlm = IDENTITY;
                }
                b"ET" => {
                    if let Some(pb) = text_clip.take() {
                        intersect_clip(canvas, &mut gs, pb.finish().as_ref(), FillRule::Winding, Transform::identity());
                    }
                }
                b"Tf" => {
                    gs.font_size = num(1);
                    gs.font = operands.first().and_then(Object::as_name).and_then(|n| self.font(resources, n));
                }
                b"Tc" => gs.char_spacing = num(0),
                b"Tw" => gs.word_spacing = num(0),
                b"Tz" => gs.h_scale = num(0) / 100.0,
                b"TL" => gs.leading = num(0),
                b"Ts" => gs.rise = num(0),
                b"Tr" => gs.render_mode = num(0) as i64,
                b"Td" | b"TD" => {
                    if op == b"TD" {
                        gs.leading = -num(1);
                    }
                    tlm = mul(&translate(num(0), num(1)), &tlm);
                    tm = tlm;
                }
                b"Tm" if operands.len() >= 6 => {
                    tlm = [num(0), num(1), num(2), num(3), num(4), num(5)];
                    tm = tlm;
                }
                b"T*" => {
                    tlm = mul(&translate(0.0, -gs.leading), &tlm);
                    tm = tlm;
                }
                b"Tj" | b"'" | b"\"" | b"TJ" => {
                    if op == b"'" || op == b"\"" {
                        if op == b"\"" {
                            gs.word_spacing = num(0);
                            gs.char_spacing = num(1);
                        }
                        tlm = mul(&translate(0.0, -gs.leading), &tlm);
                        tm = tlm;
                    }
                    if gs.render_mode >= 4 && text_clip.is_none() {
                        text_clip = Some(PathBuilder::new());
                    }
                    let items: Vec<Object> = match (op, operands.last()) {
                        (b"TJ", Some(Object::Array(a))) => a.clone(),
                        (_, Some(s @ Object::String(_))) => vec![s.clone()],
                        _ => Vec::new(),
                    };
                    for item in items {
                        match item {
                            Object::String(s) => {
                                self.show_text(canvas, &gs, &s, &mut tm, resources, text_clip.as_mut(), invisible, depth)
                            }
                            other => {
                                let tx = -other.as_f64().unwrap_or(0.0) / 1000.0 * gs.font_size * gs.h_scale;
                                tm = mul(&translate(tx, 0.0), &tm);
                            }
                        }
                    }
                }

                // --- marked content (optional content / layers) ---
                b"BMC" => hidden.push(false),
                b"BDC" => {
                    let is_oc = operands.first().and_then(Object::as_name) == Some(b"OC");
                    let hide = is_oc
                        && match operands.get(1) {
                            Some(Object::Name(n)) => doc
                                .get_dict(resources, b"Properties")
                                .and_then(|p| p.get(n).cloned())
                                .is_some_and(|o| self.layer_hidden(&o)),
                            Some(o) => self.layer_hidden(o),
                            None => false,
                        };
                    hidden.push(hide);
                }
                b"EMC" => {
                    hidden.pop();
                }
                // d0/d1 (Type 3 metrics), ri, i, BX/EX, MP/DP: nothing to draw.
                _ => {}
            }
            operands.clear();
        }
    }

    // ------------------------------------------------------------------
    // Painting
    // ------------------------------------------------------------------

    fn fill_path(&mut self, canvas: &mut Pixmap, gs: &GState, path: &Path, rule: FillRule, base: Matrix, depth: usize) {
        let Some(fill) = self.ink_fill(&gs.fill, base, depth) else { return };
        let Some((path, ts)) = safe_path(path, to_ts(&gs.ctm), canvas) else { return };
        let path = path.as_ref();
        let clip = gs.mask.as_deref();
        match fill {
            Fill::Solid(color) => {
                let paint = solid_paint(color, gs.blend);
                // Thin rectangles (TeX rules, table lines) are widened to one
                // device pixel so they don't fade out or vanish.
                if let Some(r) = device_rect(path, ts)
                    && (r.width() < 1.0 || r.height() < 1.0)
                {
                    let (cx, cy) = ((r.left() + r.right()) / 2.0, (r.top() + r.bottom()) / 2.0);
                    let (w, h) = (r.width().max(1.0), r.height().max(1.0));
                    if let Some(wide) = Rect::from_xywh(cx - w / 2.0, cy - h / 2.0, w, h) {
                        canvas.fill_rect(wide, &paint, Transform::identity(), clip);
                    }
                    return;
                }
                canvas.fill_path(path, &paint, rule, ts, clip);
            }
            Fill::Shader(shader) => {
                let Some(dev) = path.clone().transform(ts) else { return };
                let paint = Paint { shader, anti_alias: true, blend_mode: gs.blend, ..Paint::default() };
                canvas.fill_path(&dev, &paint, rule, Transform::identity(), clip);
            }
            Fill::Tile(cell, t) => {
                let Some(dev) = path.clone().transform(ts) else { return };
                let shader = Pattern::new(cell.as_ref(), SpreadMode::Repeat, FilterQuality::Bilinear, gs.fill.alpha, t);
                let paint = Paint { shader, anti_alias: true, blend_mode: gs.blend, ..Paint::default() };
                canvas.fill_path(&dev, &paint, rule, Transform::identity(), clip);
            }
        }
    }

    fn stroke_path(&mut self, canvas: &mut Pixmap, gs: &GState, path: &Path, base: Matrix, depth: usize) {
        let Some(fill) = self.ink_fill(&gs.stroke, base, depth) else { return };
        let ts = to_ts(&gs.ctm);
        // Keep very thin lines at least 0.3 device pixels wide so they
        // stay visible when zoomed out (width 0 means a 1px hairline).
        let scale = scale_of(&gs.ctm);
        let min_width = if scale > 0.0 { 0.3 / scale } else { 0.0 };
        let stroke = Stroke {
            width: if gs.line_width == 0.0 { 0.0 } else { gs.line_width.max(min_width) as f32 },
            miter_limit: gs.miter.max(1.0) as f32,
            line_cap: gs.cap,
            line_join: gs.join,
            dash: gs.dash.as_ref().and_then(|(a, p)| StrokeDash::new(a.clone(), *p)),
        };
        let clip = gs.mask.as_deref();
        // Paths far outside the page can overflow the rasterizer: outline the
        // stroke and let the (clipping) fill path handle it.
        let huge = safe_path(path, ts, canvas).is_none_or(|(p, _)| matches!(p, std::borrow::Cow::Owned(_)));
        match fill {
            Fill::Solid(color) if !huge => {
                let paint = solid_paint(color, gs.blend);
                canvas.stroke_path(path, &paint, &stroke, ts, clip);
            }
            other => {
                // Patterned strokes: outline the stroke, then fill it.
                let outline = if stroke.width == 0.0 {
                    path.stroke(&Stroke { width: (1.0 / scale_of(&gs.ctm)) as f32, ..stroke }, 1.0)
                } else {
                    path.stroke(&stroke, scale_of(&gs.ctm) as f32)
                };
                if let Some(outline) = outline {
                    let mut gs2 = gs.clone();
                    gs2.fill = gs.stroke.clone();
                    let _ = other;
                    self.fill_path(canvas, &gs2, &outline, FillRule::Winding, base, depth);
                }
            }
        }
    }

    /// How an ink paints: solid color, gradient or tiled pattern.
    fn ink_fill(&mut self, ink: &Ink, base: Matrix, depth: usize) -> Option<Fill> {
        if !matches!(*ink.space, ColorSpace::Pattern(_)) {
            let [r, g, b] = ink.rgb?;
            return Some(Fill::Solid(Color::from_rgba(r, g, b, ink.alpha)?));
        }
        let doc = self.doc;
        let pattern = doc.resolve(ink.pattern.as_ref()?).ok()?;
        let dict = pattern.as_dict()?;
        let matrix = matrix_of(doc, dict, b"Matrix").unwrap_or(IDENTITY);
        let to_device = mul(&matrix, &base);
        match doc.get_resolved(dict, b"PatternType").ok()?.as_i64()? {
            2 => {
                let shading = dict.get(b"Shading")?;
                let shader = shading::shader(doc, shading, &Dictionary::new(), to_ts(&to_device))?;
                Some(Fill::Shader(shader))
            }
            1 if depth < MAX_DEPTH => {
                let stream = pattern.as_stream()?;
                self.tile(stream, ink, to_device, depth).map(|(pm, t)| Fill::Tile(pm, t))
            }
            _ => None,
        }
    }

    /// Renders one cell of a tiling pattern.
    fn tile(&mut self, stream: &Stream, ink: &Ink, to_device: Matrix, depth: usize) -> Option<(Pixmap, Transform)> {
        let doc = self.doc;
        let dict = &stream.dict;
        let get = |k: &[u8]| doc.get_resolved(dict, k).ok().and_then(|o| o.as_f64());
        let bbox = rect_of(doc, dict, b"BBox")?;
        let xstep = get(b"XStep")?.abs();
        let ystep = get(b"YStep")?.abs();
        if xstep < 1e-6 || ystep < 1e-6 {
            return None;
        }
        // Cell resolution: device pixels per pattern unit, capped.
        let s = scale_of(&to_device);
        let cw = (xstep * s).ceil().clamp(1.0, MAX_TILE);
        let ch = (ystep * s).ceil().clamp(1.0, MAX_TILE);
        let (sx, sy) = (cw / xstep, ch / ystep);
        let cell_m: Matrix = [sx, 0.0, 0.0, -sy, -bbox[0] * sx, (bbox[1] + ystep) * sy];
        let mut cell = Pixmap::new(cw as u32, ch as u32)?;

        let mut gs = GState::new(cell_m);
        // Uncolored patterns (PaintType 2) are painted in the current color.
        if doc.get_resolved(dict, b"PaintType").ok().and_then(|o| o.as_i64()) == Some(2) {
            let mut solid = ink.clone();
            if let ColorSpace::Pattern(Some(under)) = &*ink.space {
                solid.space = Rc::new((**under).clone());
            }
            solid.pattern = None;
            gs.fill = solid.clone();
            gs.stroke = solid;
        }
        let resources = doc.get_dict(dict, b"Resources").unwrap_or_default();
        let content = doc.decode_stream(stream).ok()?;
        self.run(&mut cell, &content, &resources, gs, cell_m, depth + 1);

        let t = mul(&invert(&cell_m)?, &to_device);
        Some((cell, to_ts(&t)))
    }

    fn paint_shading(&mut self, canvas: &mut Pixmap, gs: &GState, resources: &Dictionary, name: &[u8]) {
        let doc = self.doc;
        let Some(shadings) = doc.get_dict(resources, b"Shading") else { return };
        let Some(obj) = shadings.get(name) else { return };
        let Some(shader) = shading::shader(doc, obj, resources, to_ts(&gs.ctm)) else { return };
        // Fill the shading's bbox, or everything inside the clip.
        let area = match shading::bbox(doc, obj) {
            Some([a, b, c, d]) => Rect::from_ltrb(a.min(c) as f32, b.min(d) as f32, a.max(c) as f32, b.max(d) as f32)
                .map(PathBuilder::from_rect)
                .and_then(|p| p.transform(to_ts(&gs.ctm))),
            // Only the clipped area can change: fill just its bounds.
            None => {
                let (w, h) = (canvas.width() as f32, canvas.height() as f32);
                let [l, t, r, b] = gs.clip_bounds.unwrap_or([0.0, 0.0, w, h]);
                let (l, t) = ((l - 1.0).floor().max(0.0), (t - 1.0).floor().max(0.0));
                let (r, b) = ((r + 1.0).ceil().min(w), (b + 1.0).ceil().min(h));
                Rect::from_ltrb(l, t, r, b).map(PathBuilder::from_rect)
            }
        };
        let Some(area) = area else { return };
        let Some((area, ts)) = safe_path(&area, Transform::identity(), canvas) else { return };
        let paint = Paint { shader, anti_alias: true, blend_mode: gs.blend, ..Paint::default() };
        canvas.fill_path(&area, &paint, FillRule::Winding, ts, gs.mask.as_deref());
    }

    // ------------------------------------------------------------------
    // Text
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn show_text(
        &mut self,
        canvas: &mut Pixmap,
        gs: &GState,
        bytes: &[u8],
        tm: &mut Matrix,
        resources: &Dictionary,
        mut clip: Option<&mut PathBuilder>,
        invisible: bool,
        depth: usize,
    ) {
        let Some(font) = gs.font.clone() else { return };
        let size_m: Matrix = [gs.font_size * gs.h_scale, 0.0, 0.0, gs.font_size, 0.0, gs.rise];
        let mode = gs.render_mode;
        let codes: Vec<(u32, usize)> = font.info.codes(bytes).collect();

        for (code, len) in codes {
            let trm = mul(&mul(&size_m, tm), &gs.ctm);
            if !invisible {
                if font.is_type3() {
                    if depth < MAX_DEPTH
                        && mode != 3
                        && let Some(glyph) = font.type3_glyph(self.doc, code)
                    {
                        let mut g = gs.clone();
                        g.ctm = mul(&glyph.matrix, &trm);
                        let res = glyph.resources.clone().unwrap_or_else(|| resources.clone());
                        if let Ok(content) = self.doc.decode_stream(&glyph.stream) {
                            let base = g.ctm;
                            self.run(canvas, &content, &res, g, base, depth + 1);
                        }
                    }
                } else if mode != 3
                    && mode != 7
                    && let Some(path) = font.glyph(code)
                {
                    let ts = to_ts(&trm);
                    if matches!(mode, 0 | 2 | 4 | 6) {
                        self.fill_glyph(canvas, gs, &path, ts);
                    }
                    if matches!(mode, 1 | 2 | 5 | 6)
                        && let Some(dev) = (*path).clone().transform(ts)
                    {
                        let mut g = gs.clone();
                        g.ctm = IDENTITY;
                        g.line_width *= scale_of(&gs.ctm);
                        self.stroke_path(canvas, &g, &dev, IDENTITY, depth);
                    }
                }
                if mode >= 4
                    && let (Some(pb), Some(path)) = (clip.as_deref_mut(), font.glyph(code))
                    && let Some(dev) = (*path).clone().transform(to_ts(&trm))
                {
                    pb.push_path(&dev);
                }
            }
            let mut tx = font.advance(code) * gs.font_size + gs.char_spacing;
            if code == 32 && len == 1 {
                tx += gs.word_spacing;
            }
            *tm = mul(&translate(tx * gs.h_scale, 0.0), tm);
        }
    }

    fn fill_glyph(&mut self, canvas: &mut Pixmap, gs: &GState, path: &Path, ts: Transform) {
        match &gs.fill.rgb {
            Some([r, g, b]) if !matches!(*gs.fill.space, ColorSpace::Pattern(_)) => {
                if let Some(color) = Color::from_rgba(*r, *g, *b, gs.fill.alpha)
                    && let Some((path, ts)) = safe_path(path, ts, canvas)
                {
                    canvas.fill_path(&path, &solid_paint(color, gs.blend), FillRule::Winding, ts, gs.mask.as_deref());
                }
            }
            _ => {
                // Patterned text: go through the general fill.
                if let Some(dev) = path.clone().transform(ts) {
                    let mut g = gs.clone();
                    let base = g.ctm;
                    g.ctm = IDENTITY;
                    self.fill_path(canvas, &g, &dev, FillRule::Winding, base, 0);
                }
            }
        }
    }

    fn font(&mut self, resources: &Dictionary, name: &[u8]) -> Option<Rc<RenderFont>> {
        let doc = self.doc;
        let fonts = doc.get_dict(resources, b"Font")?;
        let entry = fonts.get(name)?;
        if let Some(r) = entry.as_reference()
            && let Some(f) = self.fonts.get(&r)
        {
            return Some(f.clone());
        }
        let dict = doc.resolve(entry).ok()?.as_dict()?.clone();
        let font = Rc::new(RenderFont::load(doc, &dict, &mut self.system_fonts));
        if let Some(r) = entry.as_reference() {
            self.fonts.insert(r, font.clone());
        }
        Some(font)
    }

    // ------------------------------------------------------------------
    // XObjects, images, annotations
    // ------------------------------------------------------------------

    fn do_xobject(&mut self, canvas: &mut Pixmap, gs: &GState, resources: &Dictionary, name: &[u8], depth: usize) {
        let doc = self.doc;
        let Some(xobjects) = doc.get_dict(resources, b"XObject") else { return };
        let Ok(Object::Stream(stream)) = doc.get_resolved(&xobjects, name) else { return };
        if let Some(oc) = stream.dict.get(b"OC")
            && self.layer_hidden(oc)
        {
            return;
        }
        match stream.dict.get(b"Subtype").and_then(Object::as_name) {
            Some(b"Image") => self.draw_image(canvas, gs, &stream, resources),
            Some(b"Form") => self.draw_form(canvas, gs, &stream, resources, depth),
            _ => {}
        }
    }

    fn draw_form(&mut self, canvas: &mut Pixmap, gs: &GState, stream: &Stream, parent_resources: &Dictionary, depth: usize) {
        let doc = self.doc;
        let matrix = matrix_of(doc, &stream.dict, b"Matrix").unwrap_or(IDENTITY);
        let mut g = gs.clone();
        g.ctm = mul(&matrix, &gs.ctm);
        if let Some([a, b, c, d]) = rect_of(doc, &stream.dict, b"BBox") {
            let bbox = Rect::from_ltrb(a.min(c) as f32, b.min(d) as f32, a.max(c) as f32, b.max(d) as f32)
                .map(PathBuilder::from_rect);
            let ts = to_ts(&g.ctm);
            intersect_clip(canvas, &mut g, bbox.as_ref(), FillRule::Winding, ts);
        }
        let resources = doc.get_dict(&stream.dict, b"Resources").unwrap_or_else(|| parent_resources.clone());
        let Ok(content) = doc.decode_stream(stream) else { return };
        let base = g.ctm;

        // A transparency group is composited as a whole: draw it into its own
        // layer, then apply the outer opacity, blend mode and masks once.
        let is_group = doc
            .get_dict(&stream.dict, b"Group")
            .is_some_and(|grp| grp.get(b"S").and_then(Object::as_name) == Some(b"Transparency"));
        let needs_layer = is_group && (g.fill.alpha < 1.0 || g.blend != BlendMode::SourceOver || g.smask.is_some());
        if needs_layer && let Some(mut layer) = Pixmap::new(canvas.width(), canvas.height()) {
            let mut inner = g.clone();
            inner.fill.alpha = 1.0;
            inner.stroke.alpha = 1.0;
            inner.blend = BlendMode::SourceOver;
            inner.clip = None;
            inner.smask = None;
            inner.mask = None;
            self.run(&mut layer, &content, &resources, inner, base, depth + 1);
            let paint = PixmapPaint { opacity: g.fill.alpha, blend_mode: g.blend, quality: FilterQuality::Nearest };
            canvas.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), g.mask.as_deref());
            return;
        }
        self.run(canvas, &content, &resources, g, base, depth + 1);
    }

    fn draw_image(&mut self, canvas: &mut Pixmap, gs: &GState, stream: &Stream, resources: &Dictionary) {
        let doc = self.doc;
        let ctm = gs.ctm;
        // On-screen size of the unit square the image is drawn into.
        let target = (ctm[0].hypot(ctm[1]), ctm[2].hypot(ctm[3]));
        let is_mask = doc.get_resolved(&stream.dict, b"ImageMask").ok().and_then(|o| o.as_bool()).unwrap_or(false);
        let paint = if is_mask {
            let [r, g, b] = gs.fill.rgb.unwrap_or([0.0; 3]);
            image::Paint::Stencil([r, g, b, gs.fill.alpha])
        } else {
            image::Paint::Colors
        };
        let ts = to_ts(&ctm);
        match image::decode(doc, stream, resources, paint, target) {
            Some(img) => {
                if let Some(r) = device_bounds(canvas, &ctm) {
                    self.images.push(r.map(|v| v as f32));
                }
                if self.dark_pages && !is_mask && dark::looks_like_photo(img.data(), img.width(), img.height()) {
                    self.keep_photo(canvas, &ctm);
                }
                let (w, h) = (img.width() as f64, img.height() as f64);
                let t = mul(&[1.0 / w, 0.0, 0.0, -1.0 / h, 0.0, 1.0], &ctm);
                let paint = PixmapPaint {
                    opacity: if is_mask { 1.0 } else { gs.fill.alpha },
                    blend_mode: gs.blend,
                    quality: FilterQuality::Bilinear,
                };
                canvas.draw_pixmap(0, 0, img.as_ref(), &paint, to_ts(&t), gs.mask.as_deref());
            }
            None => {
                // Unsupported encoding (JPX/JBIG2/CCITT): show a light box.
                if let Some(r) = Rect::from_xywh(0.0, 0.0, 1.0, 1.0) {
                    let paint = solid_paint(Color::from_rgba8(220, 220, 220, 255), BlendMode::SourceOver);
                    canvas.fill_rect(r, &paint, ts, gs.mask.as_deref());
                }
            }
        }
    }

    /// Leaves the area a photo is drawn in (the unit square under `ctm`,
    /// within the clip) as it is on a dark page.
    fn keep_photo(&mut self, canvas: &Pixmap, ctm: &Matrix) {
        if let Some(keep) = device_bounds(canvas, ctm) {
            self.photos.push(keep);
        }
    }

    fn draw_annotations(&mut self, canvas: &mut Pixmap, page: &Page, resources: &Dictionary, base: Matrix) {
        let doc = self.doc;
        let Ok(Object::Array(annots)) = doc.get_resolved(&page.dict, b"Annots") else { return };
        for annot in annots {
            let Some(dict) = doc.resolve(&annot).ok().and_then(|o| o.as_dict().cloned()) else { continue };
            if dict.get(b"Subtype").and_then(Object::as_name) == Some(b"Popup") {
                continue;
            }
            let flags = doc.get_resolved(&dict, b"F").ok().and_then(|o| o.as_i64()).unwrap_or(0);
            if flags & (2 | 32) != 0 {
                continue; // Hidden or NoView
            }
            if let Some(oc) = dict.get(b"OC")
                && self.layer_hidden(oc)
            {
                continue;
            }
            // Fillable fields drawn by the viewer: only their background and
            // border, or for check boxes and radio buttons the "off" look.
            let mut state_override: Option<&[u8]> = None;
            match crate::forms::fillable(doc, &dict).filter(|_| self.hide_form_fields) {
                Some(crate::forms::Fillable::Text) => {
                    self.draw_widget_fallback(canvas, &dict, base);
                    continue;
                }
                Some(crate::forms::Fillable::Button) => state_override = Some(b"Off"),
                None => {}
            }
            let Some(ap) = doc.get_dict(&dict, b"AP") else {
                self.draw_widget_fallback(canvas, &dict, base);
                continue;
            };
            let appearance = match doc.get_resolved(&ap, b"N") {
                Ok(Object::Stream(s)) => s,
                Ok(Object::Dictionary(states)) => {
                    let Some(state) = state_override.or_else(|| dict.get(b"AS").and_then(Object::as_name)) else {
                        continue;
                    };
                    let Ok(Object::Stream(s)) = doc.get_resolved(&states, state) else { continue };
                    s
                }
                _ => continue,
            };
            let Some(rect) = rect_of(doc, &dict, b"Rect") else { continue };
            let Some(bbox) = rect_of(doc, &appearance.dict, b"BBox") else { continue };
            let matrix = matrix_of(doc, &appearance.dict, b"Matrix").unwrap_or(IDENTITY);
            // Map the transformed BBox onto the annotation rectangle.
            let corners = [(bbox[0], bbox[1]), (bbox[2], bbox[1]), (bbox[0], bbox[3]), (bbox[2], bbox[3])]
                .map(|(x, y)| (matrix[0] * x + matrix[2] * y + matrix[4], matrix[1] * x + matrix[3] * y + matrix[5]));
            let (bx0, bx1) = corners.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
            let (by0, by1) = corners.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
            if bx1 - bx0 < 1e-6 || by1 - by0 < 1e-6 {
                continue;
            }
            let (sx, sy) = ((rect[2] - rect[0]) / (bx1 - bx0), (rect[3] - rect[1]) / (by1 - by0));
            let a: Matrix = [sx, 0.0, 0.0, sy, rect[0] - bx0 * sx, rect[1] - by0 * sy];
            let gs = GState::new(mul(&a, &base));
            self.draw_form(canvas, &gs, &appearance, resources, 1);
        }
    }

    /// Form fields without an appearance stream: draw the background and
    /// border colors from `/MK`, as viewers do when generating appearances.
    fn draw_widget_fallback(&mut self, canvas: &mut Pixmap, annot: &Dictionary, base: Matrix) {
        let doc = self.doc;
        if annot.get(b"Subtype").and_then(Object::as_name) != Some(b"Widget") {
            return;
        }
        let (Some(rect), Some(mk)) = (rect_of(doc, annot, b"Rect"), doc.get_dict(annot, b"MK")) else { return };
        let color = |key: &[u8]| -> Option<Color> {
            let comps: Vec<f64> = doc.get_resolved(&mk, key).ok()?.as_array()?.iter().filter_map(Object::as_f64).collect();
            let [r, g, b] = ColorSpace::from_components(comps.len()).to_rgb(&comps)?;
            (!comps.is_empty()).then(|| Color::from_rgba(r as f32, g as f32, b as f32, 1.0)).flatten()
        };
        let Some(r) = Rect::from_ltrb(rect[0] as f32, rect[1] as f32, rect[2] as f32, rect[3] as f32) else { return };
        let ts = to_ts(&base);
        if let Some(bg) = color(b"BG") {
            canvas.fill_rect(r, &solid_paint(bg, BlendMode::SourceOver), ts, None);
        }
        if let Some(bc) = color(b"BC") {
            let width = doc
                .get_dict(annot, b"BS")
                .and_then(|bs| doc.get_resolved(&bs, b"W").ok()?.as_f64())
                .unwrap_or(1.0);
            let stroke = Stroke { width: width as f32, ..Stroke::default() };
            let inset = (width / 2.0) as f32;
            if let Some(inner) = Rect::from_ltrb(r.left() + inset, r.top() + inset, r.right() - inset, r.bottom() - inset) {
                canvas.stroke_path(&PathBuilder::from_rect(inner), &solid_paint(bc, BlendMode::SourceOver), &stroke, ts, None);
            }
        }
    }

    // ------------------------------------------------------------------
    // Graphics state parameters and optional content
    // ------------------------------------------------------------------

    fn apply_ext_gstate(&mut self, canvas: &Pixmap, gs: &mut GState, resources: &Dictionary, name: &[u8], depth: usize) {
        let doc = self.doc;
        let Some(states) = doc.get_dict(resources, b"ExtGState") else { return };
        let Some(state) = doc.get_dict(&states, name) else { return };
        for (key, value) in state.iter() {
            let value = doc.resolve(value).unwrap_or_default();
            let n = value.as_f64();
            match key {
                b"LW" => gs.line_width = n.unwrap_or(gs.line_width),
                b"ML" => gs.miter = n.unwrap_or(gs.miter),
                b"CA" => gs.stroke.alpha = n.unwrap_or(1.0).clamp(0.0, 1.0) as f32,
                b"ca" => gs.fill.alpha = n.unwrap_or(1.0).clamp(0.0, 1.0) as f32,
                b"D" => {
                    if let Some(a) = value.as_array()
                        && let Some(Object::Array(dashes)) = a.first()
                    {
                        gs.dash = make_dash(dashes, a.get(1).and_then(Object::as_f64).unwrap_or(0.0));
                    }
                }
                b"SMask" => {
                    gs.smask = match &value {
                        Object::Dictionary(d) if depth < MAX_DEPTH => self.soft_mask(canvas, gs, d, resources, depth),
                        _ => None, // /None
                    };
                    gs.refresh_mask();
                }
                b"BM" => {
                    let name = match &value {
                        Object::Array(a) => a.first().and_then(Object::as_name).map(<[u8]>::to_vec),
                        other => other.as_name().map(<[u8]>::to_vec),
                    };
                    gs.blend = blend_mode(name.as_deref().unwrap_or(b"Normal"));
                }
                b"Font" => {
                    if let Some(a) = value.as_array()
                        && let Some(Object::Reference(r)) = a.first()
                    {
                        gs.font_size = a.get(1).and_then(Object::as_f64).unwrap_or(gs.font_size);
                        if let Ok(dict) = doc.get_object(*r)
                            && let Some(d) = dict.as_dict()
                        {
                            let font = self
                                .fonts
                                .entry(*r)
                                .or_insert_with(|| Rc::new(RenderFont::load(doc, d, &mut self.system_fonts)))
                                .clone();
                            gs.font = Some(font);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Renders a soft mask group (`/SMask` in an ExtGState) into a mask.
    fn soft_mask(&mut self, canvas: &Pixmap, gs: &GState, smask: &Dictionary, resources: &Dictionary, depth: usize) -> Option<Rc<Mask>> {
        let doc = self.doc;
        let Ok(Object::Stream(group)) = doc.get_resolved(smask, b"G") else { return None };
        let luminosity = smask.get(b"S").and_then(Object::as_name) != Some(b"Alpha");
        let mut layer = Pixmap::new(canvas.width(), canvas.height())?;
        if luminosity {
            // The backdrop (/BC, default black) shows where the group paints nothing.
            let bc: Vec<f64> = doc
                .get_resolved(smask, b"BC")
                .ok()
                .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
                .unwrap_or_default();
            let [r, g, b] = if bc.is_empty() { [0.0; 3] } else { ColorSpace::from_components(bc.len()).to_rgb(&bc)? };
            layer.fill(Color::from_rgba(r as f32, g as f32, b as f32, 1.0)?);
        }
        let mut inner = GState::new(gs.ctm);
        inner.font = gs.font.clone();
        self.draw_form(&mut layer, &inner, &group, resources, depth + 1);
        let kind = if luminosity { tiny_skia::MaskType::Luminance } else { tiny_skia::MaskType::Alpha };
        Some(Rc::new(Mask::from_pixmap(layer.as_ref(), kind)))
    }

    /// Whether an optional content group or membership dict is hidden.
    fn layer_hidden(&self, obj: &Object) -> bool {
        let doc = self.doc;
        if let Some(r) = obj.as_reference()
            && self.hidden_layers.contains(&r)
        {
            return true;
        }
        let Ok(resolved) = doc.resolve(obj) else { return false };
        let Some(dict) = resolved.as_dict() else { return false };
        if dict.type_name() != Some(b"OCMD") {
            return false;
        }
        // Membership dictionary: visibility from its groups and policy.
        let groups: Vec<ObjRef> = match dict.get(b"OCGs") {
            Some(Object::Reference(r)) => vec![*r],
            Some(Object::Array(a)) => a.iter().filter_map(Object::as_reference).collect(),
            _ => return false,
        };
        let on = groups.iter().map(|g| !self.hidden_layers.contains(g));
        let visible = match dict.get(b"P").and_then(Object::as_name) {
            Some(b"AllOn") => on.clone().all(|v| v),
            Some(b"AnyOff") => on.clone().any(|v| !v),
            Some(b"AllOff") => on.clone().all(|v| !v),
            _ => on.clone().any(|v| v) || groups.is_empty(),
        };
        !visible
    }
}

/// Layers switched off in the default view configuration.
fn hidden_layers(doc: &Document) -> HashSet<ObjRef> {
    let mut hidden = HashSet::default();
    let Ok(catalog) = doc.catalog() else { return hidden };
    let Some(props) = doc.get_dict(&catalog, b"OCProperties") else { return hidden };
    let Some(d) = doc.get_dict(&props, b"D") else { return hidden };
    let refs = |dict: &Dictionary, key: &[u8]| -> Vec<ObjRef> {
        match doc.get_resolved(dict, key) {
            Ok(Object::Array(a)) => a.iter().filter_map(Object::as_reference).collect(),
            _ => Vec::new(),
        }
    };
    if d.get(b"BaseState").and_then(Object::as_name) == Some(b"OFF") {
        let on: HashSet<ObjRef> = refs(&d, b"ON").into_iter().collect();
        hidden.extend(refs(&props, b"OCGs").into_iter().filter(|r| !on.contains(r)));
    }
    hidden.extend(refs(&d, b"OFF"));
    hidden
}

// ---------------------------------------------------------------------------
// Graphics state
// ---------------------------------------------------------------------------

/// A paint source: color space, components and opacity.
#[derive(Clone)]
struct Ink {
    space: Rc<ColorSpace>,
    comps: Vec<f64>,
    /// Cached sRGB; `None` when nothing should be painted.
    rgb: Option<[f32; 3]>,
    alpha: f32,
    /// The pattern object for the Pattern color space.
    pattern: Option<Object>,
}

impl Ink {
    fn black() -> Ink {
        Ink { space: Rc::new(ColorSpace::Gray), comps: vec![0.0], rgb: Some([0.0; 3]), alpha: 1.0, pattern: None }
    }

    fn set_space(&mut self, cs: ColorSpace) {
        self.comps = cs.initial_color();
        self.space = Rc::new(cs);
        self.pattern = None;
        self.update();
    }

    fn set_components(&mut self, comps: Vec<f64>) {
        if !comps.is_empty() || self.pattern.is_some() {
            self.comps = comps;
        }
        self.update();
    }

    fn update(&mut self) {
        self.rgb = self.space.to_rgb(&self.comps).map(|c| c.map(|v| v as f32));
    }
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    fill: Ink,
    stroke: Ink,
    line_width: f64,
    cap: LineCap,
    join: LineJoin,
    miter: f64,
    dash: Option<(Vec<f32>, f32)>,
    /// Geometric clip (W/W*, form BBox).
    clip: Option<Rc<Mask>>,
    /// Device-space bounds of `clip` (left, top, right, bottom), so painting
    /// that would cover the whole page (`sh` without a `/BBox`) only covers
    /// the clipped area.
    clip_bounds: Option<[f32; 4]>,
    /// Soft mask from an ExtGState `/SMask`.
    smask: Option<Rc<Mask>>,
    /// clip × smask: what painting operations are masked with.
    mask: Option<Rc<Mask>>,
    blend: BlendMode,
    font: Option<Rc<RenderFont>>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
    render_mode: i64,
}

impl GState {
    /// Recomputes `mask` after `clip` or `smask` changed.
    fn refresh_mask(&mut self) {
        self.mask = match (&self.clip, &self.smask) {
            (None, None) => None,
            (Some(m), None) | (None, Some(m)) => Some(m.clone()),
            (Some(c), Some(s)) => {
                let mut m = (**c).clone();
                for (a, b) in m.data_mut().iter_mut().zip(s.data()) {
                    *a = ((*a as u16 * *b as u16 + 127) / 255) as u8;
                }
                Some(Rc::new(m))
            }
        };
    }

    fn new(ctm: Matrix) -> GState {
        GState {
            ctm,
            fill: Ink::black(),
            stroke: Ink::black(),
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 10.0,
            dash: None,
            clip: None,
            clip_bounds: None,
            smask: None,
            mask: None,
            blend: BlendMode::SourceOver,
            font: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
        }
    }
}

enum Fill {
    Solid(Color),
    /// A gradient already positioned in device space.
    Shader(Shader<'static>),
    /// A pattern cell and the transform that tiles it over the page.
    Tile(Pixmap, Transform),
}

fn solid_paint(color: Color, blend: BlendMode) -> Paint<'static> {
    let mut paint = Paint { anti_alias: true, blend_mode: blend, ..Paint::default() };
    paint.set_color(color);
    paint
}

/// Intersects the clip with `path`. A `None` path clips everything away.
fn intersect_clip(canvas: &Pixmap, gs: &mut GState, path: Option<&Path>, rule: FillRule, ts: Transform) {
    let (w, h) = (canvas.width(), canvas.height());
    let Some(path) = path else {
        gs.clip = Mask::new(w, h).map(Rc::new);
        gs.clip_bounds = Some([0.0; 4]);
        gs.refresh_mask();
        return;
    };
    // Common case: a rectangle that covers the whole page clips nothing.
    if covers_canvas(path, ts, w, h) {
        return;
    }
    let Some((path, ts)) = safe_path(path, ts, canvas) else {
        gs.clip = Mask::new(w, h).map(Rc::new);
        gs.clip_bounds = Some([0.0; 4]);
        gs.refresh_mask();
        return;
    };
    let path = path.as_ref();
    let b = transformed_bounds(path.bounds(), ts);
    let [l, t, r, bt] = gs.clip_bounds.unwrap_or([0.0, 0.0, w as f32, h as f32]);
    gs.clip_bounds = Some([b[0].max(l), b[1].max(t), b[2].min(r).max(b[0].max(l)), b[3].min(bt).max(b[1].max(t))]);
    let mask = match &gs.clip {
        Some(existing) => {
            let mut m = (**existing).clone();
            m.intersect_path(path, rule, true, ts);
            m
        }
        None => {
            let Some(mut m) = Mask::new(w, h) else { return };
            m.fill_path(path, rule, true, ts);
            m
        }
    };
    gs.clip = Some(Rc::new(mask));
    gs.refresh_mask();
}

/// The bounding box of `bounds` transformed by `ts`.
fn transformed_bounds(bounds: Rect, ts: Transform) -> [f32; 4] {
    let mut out = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    for (x, y) in [(bounds.left(), bounds.top()), (bounds.right(), bounds.top()), (bounds.left(), bounds.bottom()), (bounds.right(), bounds.bottom())] {
        let dx = ts.sx * x + ts.kx * y + ts.tx;
        let dy = ts.ky * x + ts.sy * y + ts.ty;
        out = [out[0].min(dx), out[1].min(dy), out[2].max(dx), out[3].max(dy)];
    }
    out
}

/// Device coordinates beyond this distance from the canvas get clipped
/// before rasterizing (tiny-skia uses fixed-point internally).
const SAFE_COORD: f32 = 16384.0;

/// Returns the path ready to rasterize: unchanged if its device bounds are
/// reasonable, otherwise flattened and clipped to the canvas (in device
/// space, with an identity transform). `None` if it has no usable geometry.
fn safe_path<'p>(path: &'p Path, ts: Transform, canvas: &Pixmap) -> Option<(std::borrow::Cow<'p, Path>, Transform)> {
    let b = path.bounds();
    let corners = [(b.left(), b.top()), (b.right(), b.top()), (b.left(), b.bottom()), (b.right(), b.bottom())];
    let (w, h) = (canvas.width() as f32, canvas.height() as f32);
    let mut ok = true;
    for (x, y) in corners {
        let dx = ts.sx * x + ts.kx * y + ts.tx;
        let dy = ts.ky * x + ts.sy * y + ts.ty;
        if !dx.is_finite() || !dy.is_finite() {
            return None;
        }
        ok &= dx > -SAFE_COORD && dy > -SAFE_COORD && dx < w + SAFE_COORD && dy < h + SAFE_COORD;
    }
    if ok {
        return Some((std::borrow::Cow::Borrowed(path), ts));
    }
    let dev = path.clone().transform(ts)?;
    let clipped = clip_to_rect(&dev, -2.0, -2.0, w + 2.0, h + 2.0)?;
    Some((std::borrow::Cow::Owned(clipped), Transform::identity()))
}

/// Flattens `path` and clips every subpath to a rectangle
/// (Sutherland–Hodgman). Filling the result matches filling the original
/// inside the rectangle.
fn clip_to_rect(path: &Path, x0: f32, y0: f32, x1: f32, y1: f32) -> Option<Path> {
    use tiny_skia::{PathSegment, Point};
    let mut polys: Vec<Vec<Point>> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    let mut last = Point::zero();
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                if cur.len() > 2 {
                    polys.push(std::mem::take(&mut cur));
                }
                cur.clear();
                cur.push(p);
                last = p;
            }
            PathSegment::LineTo(p) => {
                cur.push(p);
                last = p;
            }
            PathSegment::QuadTo(c, p) => {
                for i in 1..=8 {
                    let t = i as f32 / 8.0;
                    let u = 1.0 - t;
                    cur.push(Point::from_xy(
                        u * u * last.x + 2.0 * u * t * c.x + t * t * p.x,
                        u * u * last.y + 2.0 * u * t * c.y + t * t * p.y,
                    ));
                }
                last = p;
            }
            PathSegment::CubicTo(c1, c2, p) => {
                for i in 1..=16 {
                    let t = i as f32 / 16.0;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    cur.push(Point::from_xy(
                        a * last.x + b * c1.x + c * c2.x + d * p.x,
                        a * last.y + b * c1.y + c * c2.y + d * p.y,
                    ));
                }
                last = p;
            }
            PathSegment::Close => {}
        }
    }
    if cur.len() > 2 {
        polys.push(cur);
    }

    // Clip against one edge: keep points where `inside` holds.
    fn clip_edge(poly: &[Point], inside: impl Fn(Point) -> bool, cross: impl Fn(Point, Point) -> Point) -> Vec<Point> {
        let mut out = Vec::with_capacity(poly.len() + 4);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            match (inside(a), inside(b)) {
                (true, true) => out.push(b),
                (true, false) => out.push(cross(a, b)),
                (false, true) => {
                    out.push(cross(a, b));
                    out.push(b);
                }
                (false, false) => {}
            }
        }
        out
    }
    let at_x = |a: Point, b: Point, x: f32| Point::from_xy(x, a.y + (b.y - a.y) * (x - a.x) / (b.x - a.x));
    let at_y = |a: Point, b: Point, y: f32| Point::from_xy(a.x + (b.x - a.x) * (y - a.y) / (b.y - a.y), y);

    let mut pb = PathBuilder::new();
    for poly in polys {
        let p = clip_edge(&poly, |q| q.x >= x0, |a, b| at_x(a, b, x0));
        let p = clip_edge(&p, |q| q.x <= x1, |a, b| at_x(a, b, x1));
        let p = clip_edge(&p, |q| q.y >= y0, |a, b| at_y(a, b, y0));
        let p = clip_edge(&p, |q| q.y <= y1, |a, b| at_y(a, b, y1));
        if p.len() > 2 {
            pb.move_to(p[0].x, p[0].y);
            for q in &p[1..] {
                pb.line_to(q.x, q.y);
            }
            pb.close();
        }
    }
    pb.finish()
}

fn covers_canvas(path: &Path, ts: Transform, w: u32, h: u32) -> bool {
    device_rect(path, ts)
        .is_some_and(|b| b.left() <= 0.0 && b.top() <= 0.0 && b.right() >= w as f32 && b.bottom() >= h as f32)
}

/// If `path` is a single axis-aligned rectangle in device space, returns it.
fn device_rect(path: &Path, ts: Transform) -> Option<Rect> {
    // A rectangle is move + 3 lines (+ optional line back) + close.
    if path.len() > 6 || path.points().len() > 5 || !(ts.kx == 0.0 && ts.ky == 0.0) {
        return None;
    }
    let dev = path.clone().transform(ts)?;
    let b = dev.bounds();
    let corner = |v: f32, a: f32, c: f32| (v - a).abs() < 0.01 || (v - c).abs() < 0.01;
    dev.points()
        .iter()
        .all(|p| corner(p.x, b.left(), b.right()) && corner(p.y, b.top(), b.bottom()))
        .then_some(b)
}

fn make_dash(array: &[Object], phase: f64) -> Option<(Vec<f32>, f32)> {
    let mut v: Vec<f32> = array.iter().filter_map(Object::as_f64).map(|x| x.max(0.0) as f32).collect();
    if v.is_empty() || v.iter().all(|&x| x == 0.0) {
        return None;
    }
    if v.len() % 2 == 1 {
        v.extend_from_within(..);
    }
    Some((v, phase as f32))
}

fn blend_mode(name: &[u8]) -> BlendMode {
    match name {
        b"Multiply" => BlendMode::Multiply,
        b"Screen" => BlendMode::Screen,
        b"Overlay" => BlendMode::Overlay,
        b"Darken" => BlendMode::Darken,
        b"Lighten" => BlendMode::Lighten,
        b"ColorDodge" => BlendMode::ColorDodge,
        b"ColorBurn" => BlendMode::ColorBurn,
        b"HardLight" => BlendMode::HardLight,
        b"SoftLight" => BlendMode::SoftLight,
        b"Difference" => BlendMode::Difference,
        b"Exclusion" => BlendMode::Exclusion,
        b"Hue" => BlendMode::Hue,
        b"Saturation" => BlendMode::Saturation,
        b"Color" => BlendMode::Color,
        b"Luminosity" => BlendMode::Luminosity,
        _ => BlendMode::SourceOver,
    }
}

/// The pixels covered by the unit square under `ctm` (an image), within
/// the canvas: `[left, top, right, bottom]`, or `None` if it's off the page.
fn device_bounds(canvas: &Pixmap, ctm: &Matrix) -> Option<[u32; 4]> {
    let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
        .map(|(x, y)| (x * ctm[0] + y * ctm[2] + ctm[4], x * ctm[1] + y * ctm[3] + ctm[5]));
    let (x0, x1) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.0), b.max(c.0)));
    let (y0, y1) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.1), b.max(c.1)));
    let clamp = |v: f64, max: u32| v.round().clamp(0.0, max as f64) as u32;
    let r = [clamp(x0, canvas.width()), clamp(y0, canvas.height()), clamp(x1, canvas.width()), clamp(y1, canvas.height())];
    (r[2] > r[0] && r[3] > r[1]).then_some(r)
}

/// Reads `BI <dict> ID <data> EI`; the parser is positioned after `BI`.
fn read_inline_image(parser: &mut Parser) -> Option<Stream> {
    let mut dict = Dictionary::new();
    loop {
        match parser.lex.next_token()? {
            Token::Keyword(b"ID") => break,
            Token::Name(key) => {
                let value = parser.parse_object().ok()?;
                dict.set(expand_inline_key(&key).to_vec(), value);
            }
            _ => {}
        }
    }
    let data = parser.lex.data();
    let start = parser.lex.pos() + 1; // one whitespace byte after ID
    // Without filters the exact size is known; otherwise look for `EI`.
    let known_len = if dict.contains(b"Filter") {
        None
    } else {
        let get = |k: &[u8]| dict.get(k).and_then(Object::as_i64).unwrap_or(0).max(0) as usize;
        let mask = dict.get(b"ImageMask").and_then(Object::as_bool).unwrap_or(false);
        let comps = if mask {
            1
        } else {
            match dict.get(b"ColorSpace").and_then(Object::as_name) {
                Some(b"DeviceRGB" | b"RGB") => 3,
                Some(b"DeviceCMYK" | b"CMYK") => 4,
                _ => 1,
            }
        };
        let bpc = if mask { 1 } else { get(b"BitsPerComponent").max(1) };
        Some((get(b"Width") * comps * bpc).div_ceil(8) * get(b"Height"))
    };
    let is_ws = |c: u8| crate::object::is_whitespace(c);
    let ei_at = |p: usize| {
        data.get(p..p + 2) == Some(b"EI") && data.get(p + 2).is_none_or(|&c| is_ws(c) || c == b'Q')
    };
    let end = match known_len {
        Some(n) if start + n <= data.len() && {
            let mut p = start + n;
            while data.get(p).is_some_and(|&c| is_ws(c)) {
                p += 1;
            }
            ei_at(p)
        } =>
        {
            start + n
        }
        _ => {
            let mut p = start;
            while p + 1 < data.len() && !(ei_at(p) && p > start && is_ws(data[p - 1])) {
                p += 1;
            }
            p.min(data.len())
        }
    };
    let raw = data.get(start..end)?.to_vec();
    // Resume after `EI`.
    let mut resume = end;
    while resume < data.len() && !ei_at(resume) {
        resume += 1;
    }
    parser.lex.set_pos(resume + 2);
    Some(Stream { dict, raw: raw.into() })
}

fn expand_inline_key(key: &[u8]) -> &[u8] {
    match key {
        b"BPC" => b"BitsPerComponent",
        b"CS" => b"ColorSpace",
        b"D" => b"Decode",
        b"DP" => b"DecodeParms",
        b"F" => b"Filter",
        b"H" => b"Height",
        b"IM" => b"ImageMask",
        b"I" => b"Interpolate",
        b"W" => b"Width",
        b"L" => b"Length",
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Matrices
// ---------------------------------------------------------------------------

/// `m` then `n` (PDF row-vector convention: m × n).
fn mul(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

fn translate(tx: f64, ty: f64) -> Matrix {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)])
}

/// Average linear scale factor of a matrix.
fn scale_of(m: &Matrix) -> f64 {
    (m[0] * m[3] - m[1] * m[2]).abs().sqrt()
}

fn to_ts(m: &Matrix) -> Transform {
    Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, m[4] as f32, m[5] as f32)
}

fn matrix_of(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<Matrix> {
    let v: Vec<f64> = doc.get_resolved(dict, key).ok()?.as_array()?.iter().filter_map(Object::as_f64).collect();
    (v.len() == 6).then(|| [v[0], v[1], v[2], v[3], v[4], v[5]])
}

fn rect_of(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<[f64; 4]> {
    let v: Vec<f64> = doc
        .get_resolved(dict, key)
        .ok()?
        .as_array()?
        .iter()
        .filter_map(|o| doc.resolve(o).ok()?.as_f64())
        .collect();
    (v.len() == 4).then(|| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_math() {
        let m = [2.0, 0.0, 0.0, 3.0, 10.0, 20.0];
        let inv = invert(&m).unwrap();
        let id = mul(&m, &inv);
        for (a, b) in id.iter().zip(IDENTITY.iter()) {
            assert!((a - b).abs() < 1e-12);
        }
        assert_eq!(scale_of(&[2.0, 0.0, 0.0, 2.0, 0.0, 0.0]), 2.0);
    }

    #[test]
    fn huge_paths_are_clipped_to_the_canvas() {
        let canvas = Pixmap::new(100, 100).unwrap();
        let big = PathBuilder::from_rect(Rect::from_ltrb(-1e11, -1e11, 1e11, 1e11).unwrap());
        let (clipped, ts) = safe_path(&big, Transform::identity(), &canvas).unwrap();
        assert!(matches!(clipped, std::borrow::Cow::Owned(_)));
        assert!(ts.is_identity());
        let b = clipped.bounds();
        assert_eq!((b.left(), b.top(), b.right(), b.bottom()), (-2.0, -2.0, 102.0, 102.0));
        // Ordinary paths pass through untouched.
        let small = PathBuilder::from_rect(Rect::from_ltrb(10.0, 10.0, 20.0, 20.0).unwrap());
        assert!(matches!(safe_path(&small, Transform::identity(), &canvas), Some((std::borrow::Cow::Borrowed(_), _))));
        // Paths completely outside the clip window vanish.
        let away = PathBuilder::from_rect(Rect::from_ltrb(1e9, 1e9, 2e9, 2e9).unwrap());
        assert!(clip_to_rect(&away, 0.0, 0.0, 100.0, 100.0).is_none());
    }

    #[test]
    fn dash_patterns() {
        assert_eq!(make_dash(&[Object::Integer(3)], 0.0), Some((vec![3.0, 3.0], 0.0)));
        assert_eq!(make_dash(&[], 0.0), None);
    }
}
