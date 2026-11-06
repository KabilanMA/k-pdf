# k-pdf

A small, low-memory PDF library, CLI and desktop viewer written in Rust.

The core reader depends only on `flate2` (pure-Rust deflate) and `memmap2`. Rendering is an optional feature (`render`, on by default) that adds `tiny-skia` (rasterizer), `ttf-parser` (fonts), `jpeg-decoder` and `fontdb` (system font lookup).

### Features

**Phase 1 — Reader**
- Parses PDF 1.0 through 2.0
- Cross-reference tables (classic), cross-reference streams and object streams (PDF 1.5+), hybrid files, incremental updates (`/Prev` chains)
- Repairs damaged files: wrong offsets, broken or missing `startxref`, missing xref/trailer, truncated files, junk before `%PDF`, wrong stream `/Length`. If needed it rebuilds the object table by scanning the file. If the catalog is lost, it builds one around the surviving page tree.
- Stream filters: FlateDecode, LZWDecode, ASCII85Decode, ASCIIHexDecode, RunLengthDecode, CCITTFaxDecode (Group 3/4), BrotliDecode (PDF 2.0 extension, 2026), PNG/TIFF predictors. JPEG is decoded by the renderer; JPX and JBIG2 are passed through still encoded.
- Page tree walking with inherited attributes (`Resources`, `MediaBox`, `CropBox`, `Rotate`) and cycle protection
- Document metadata (`/Info`), with PDFDocEncoding, UTF-16 and UTF-8 text strings
- Plain-text extraction: ToUnicode CMaps, Standard/WinAnsi/MacRoman encodings with `/Differences`, Type0/CID fonts, Type3 fonts, Form XObjects and inline images. Spaces and line breaks come from glyph positions, and "fake bold" overstrikes are removed.

**Phase 2 — Renderer and viewer**
- **Graphics:** paths, fills, strokes (caps, joins, dashes), clipping (including text clipping), blend modes
- **Transparency:** constant alpha, transparency groups, luminosity and alpha soft masks
- **Colors:** DeviceGray/RGB/CMYK, ICCBased (by component count), CalRGB/CalGray, Lab, Indexed, Separation and DeviceN, with PDF functions of types 0, 2, 3 and 4 (PostScript calculator)
- **Shadings and patterns:** axial and radial shadings (`sh` and shading patterns), colored and uncolored tiling patterns
- **Images:** raw samples at 1–16 bits per component, JPEG (including Adobe CMYK), CCITT fax, stencil masks, `/SMask`, `/Mask` (stencil or color key), `/Decode` arrays, inline images
- **Fonts:** embedded TrueType, OpenType, CFF (Type1C and CID-keyed), classic Type 1 (eexec, flex, `seac`) and Type 3. Non-embedded fonts are replaced by a similar system font, stretched to the PDF's glyph widths. The standard 14 fonts use Adobe's built-in metrics when a PDF doesn't list widths.
- **Pages:** form XObjects, annotation appearance streams (with a background/border fallback for form fields that have none), optional content (hidden layers), `/Rotate` and `/CropBox`
- **Desktop viewer** (`viewer/`, using egui):
  - Opens files via a dialog, drag-and-drop or the command line.
  - A modern look:
    - dark and light themes, which follow the system setting or can be chosen in the "⋯" menu;
    - the desktop's own UI font when one is found (Adwaita Sans, Inter, Noto Sans, Cantarell, Segoe UI);
    - crisp vector icons, soft shadows under pages and floating panels.

    It costs nothing at startup: no assets are bundled and nothing is animated in the background. The only extra is one font file read, adding about 2.5 MB of memory.
  - The toolbar doubles as the title bar:
    - Open, Save, Undo and Redo on the left;
    - the editing tools (Edit text, Crop, Redact) as one group;
    - the file name in the middle, with a dot for unsaved changes;
    - a "⋯" menu (Save as, Compress, Document info, Appearance) and round window buttons on the right.

    Drag it to move the window, double-click it to maximize, and resize from any edge. In narrow windows the buttons show icons only, and then Undo and Redo move into the menu.
  - Page navigation and zoom float at the bottom of the page view. Click the zoom percentage for Fit width, Fit page and fixed sizes.
  - A page sidebar with thumbnails of every page: it follows the main view, highlights the current page, and clicking a thumbnail goes to that page. It can be resized or hidden (sidebar button).
  - The welcome screen opens a file, and dragging a file over the window shows where to drop it.
  - Editing text (Edit mode, Ctrl+E): click a paragraph to edit it in place, as in a word processor. The text keeps its font, size and color; a toolbar changes the font, size, bold, italic, underline and color of the selection (or of what you type next), and the paragraph's alignment, line spacing and width. Text re-wraps to the paragraph's width. Click an empty spot to add a new text box; delete all of a paragraph's text to remove it.
  - Cropping (Crop mode): drag on a page to choose the area to keep (adjust it with the handles, or drag it to move it), then crop that page, every page, or the pages you list (e.g. "1, 3-5") to the same area. Crops can be removed per page or all at once; Ctrl+Z / Ctrl+Shift+Z undo and redo them.
  - Redacting (Redact mode): drag over anything on a page to mark it, or type words to mark every place they appear; click a mark to unmark it. "Redact" then removes what is under the marks for good (text, image pixels, shapes, links and notes) and draws black boxes in its place. Ctrl+Z undoes it until the file is saved.
  - Filling in forms: text fields (single and multi-line, password, length limits), check boxes, radio buttons and drop-down lists are editable on the page. Save writes the values into the file, with appearances that every PDF viewer shows.
  - Inserting PDFs: hovering the gap between two thumbnails (or above the first, or below the last) shows a "+" button; click it and pick a PDF to insert its pages there.
  - Compressing ("Compress…", next to Save as): saves a smaller copy, including unsaved changes, that looks and works exactly the same. JPEG optimization is on by default. Brotli is an option that is off by default; ticking it shows a warning that many PDF readers can't open such files. The dialog then shows how much smaller the copy is, and can open it.
  - Rearranging pages: drag a thumbnail in the sidebar to move the page. Save (Ctrl+S) writes the new order to the file, Save as (Ctrl+Shift+S) to a copy, and Ctrl+Z undoes a move or an insertion, and Ctrl+Shift+Z (or Ctrl+Y) redoes it. Closing the window or opening another file with unsaved changes asks whether to save them.
  - Continuous scrolling, zoom (the floating controls, Ctrl+wheel, Ctrl +/-) and page jumping.
  - Text selection and copying. Selection follows reading order: it stays within a column and skips margin line numbers (as in review copies of papers).
  - Links: double-click follows a link within the document (contents, sections, figures, citations); web links open in the browser only with Ctrl+click. Hovering shows where a link goes, and Alt+← (or the mouse back button) returns.
  - A document info window: a separate window (it can be moved outside the viewer), resizable from its edges and corners, with long values wrapped.
  - Several threads render pages in parallel, and the pages just ahead in the scroll direction are prepared in advance.

- **Links:** `page_links` returns a page's links with their rectangles and targets: web addresses, or a page and position in the document (go-to actions, explicit and named destinations, next/previous/first/last page).
- **Cropping:** `edit::crop_page` sets the visible area of a page (its `/CropBox`), given in points as the page is shown, or removes the crop.
- **Redaction:** `edit::redact_page` removes what is in areas of a page and blacks them out. Text is taken out of the content, with the rest of each line kept in place. Image pixels are blacked out in a new copy of the image. Shapes lying inside an area and annotations over it are removed. Reused content (form XObjects) is redacted in a copy for that page. `edit::find_text` finds the areas covering a phrase on a page.
- **Lossless compression:** `compress::compress` writes a smaller copy without changing anything it shows or contains:
  - every stream is re-encoded at libdeflate's highest level, with PNG row prediction for images, or left as it is when that isn't smaller;
  - older encodings (LZW, ASCII85 and others) are converted along the way;
  - JPEG images are re-encoded losslessly with MozJPEG (optimized tables or progressive scans, same image data), and each one is checked to decode to identical pixels;
  - identical objects are merged, and unused ones dropped;
  - objects are packed into large object streams with a compact cross-reference stream.

  Streams that don't decode cleanly, and XMP metadata (kept uncompressed for PDF/A), are copied unchanged. With `CompressOptions::brotli`, streams may use Brotli instead. That makes smaller files, which only recent readers can open: they're marked PDF 2.0, with the PDF Association's registered Brotli extension (`/Extensions << /PDFa [...] >>`).
- **Text editing:** `edit::page_text` finds a page's text as paragraphs, with each character's font, size and color; `edit::edit_text` replaces a paragraph (or adds one) with styled text, word-wrapped and aligned. The original characters are taken out of the page content and the new text is drawn in their place, in the original font when it has every character needed, otherwise in a matching installed font (embedded; TrueType fonts with only the glyphs used) or a standard PDF font.
- **Forms:** `form_fields` lists a document's fields (kind, value, options, flags, and where each appears); `fill_form` turns new values into changes for `write_document_with_changes`, including new appearance streams for text and choice fields and on/off states for buttons.
- **Writing:** `write_with_page_order` saves a document with its pages rearranged (or some left out), and `write_document` combines pages from several documents (merging). Pages taken from other documents bring along only what they use (content, fonts, images, annotations), renumbered. The file is rewritten from scratch: objects are copied unchanged (streams stay compressed), the page tree is flattened with inherited attributes copied into each page, and PDF 1.5+ files are written with compressed object streams and a cross-reference stream. Only objects still in use are written, so what edits and redactions replaced doesn't stay in the file. Damaged files come out repaired.
- **Text positions:** `TextExtractor::extract_chars` returns each character with its rectangle on the displayed page, which is what text selection uses.

**Low memory by design:**
- The file is memory-mapped, not read into RAM, and objects are parsed only when requested.
- Images are decoded at roughly their on-screen size, not their full resolution.
- The sidebar renders thumbnails only for the part of the list on screen, after the main view's pages, and keeps at most 400 of them.
- The viewer renders in the background. It keeps recently shown pages up to a 256 MB texture budget and drops the least recently shown ones first.
- Extracting the text of a 384-page manual peaks at about 5.5 MB RSS.
- The viewer with a 24 MB PDF open peaks at about 89 MB RSS. About 69 MB of that is the window and OpenGL baseline before any file is opened.

### Installing

The application is called **k-pdf**; the command-line tool is **k-pdf-cli**.

From a release: download `k-pdf-<version>-linux-x86_64.tar.gz` from the GitHub releases page, unpack it and run:

```sh
./install.sh             # into ~/.local: k-pdf appears in the applications menu
./install.sh --default   # …and opens PDF files by default
./install.sh --uninstall
sudo PREFIX=/usr/local ./install.sh   # for all users
```

From source: `cargo build --release --workspace`, then `packaging/linux/install.sh` (same options).

Releasing: `packaging/linux/package.sh` builds the archive into `dist/`. Pushing a version tag (`git tag v0.1.0 && git push origin v0.1.0`) makes GitHub Actions (`.github/workflows/release.yml`) test, build and publish it as a release. The build runs on Ubuntu 22.04, so it runs on Linux systems from 2022 on.

### Usage

```sh
cargo build --release --workspace
./target/release/k-pdf [file.pdf] [--page N]            # the application

./target/release/k-pdf-cli info   file.pdf
./target/release/k-pdf-cli text   file.pdf [--page N]
./target/release/k-pdf-cli object file.pdf <num> [gen] [--decode]
./target/release/k-pdf-cli render file.pdf <page> out.png [--scale S]
./target/release/k-pdf-cli reorder in.pdf out.pdf 3,1,2      # or ranges: 5-10,1-4
./target/release/k-pdf-cli merge   out.pdf a.pdf b.pdf ...
./target/release/k-pdf-cli crop    in.pdf out.pdf 40 30 572 400 [1,3-5]   # left top right bottom
./target/release/k-pdf-cli redact  in.pdf out.pdf 1 72 90 300 120         # page left top right bottom
./target/release/k-pdf-cli redact  in.pdf out.pdf --text "Jane Doe" [1,3-5]
./target/release/k-pdf-cli compress in.pdf out.pdf [--no-jpeg] [--brotli]   # lossless; --brotli: see below
./target/release/k-pdf-cli blocks  file.pdf 1                 # list a page's text blocks
./target/release/k-pdf-cli edit    in.pdf out.pdf 1 4 "New text for block 4"
./target/release/k-pdf-cli form    form.pdf                   # list the fields
./target/release/k-pdf-cli fill    form.pdf out.pdf "Name=Ada Lovelace" "Agree=on"
```

Viewer keys:

| Keys | Action |
|---|---|
| Ctrl+O | Open a file |
| PageUp / PageDown, ← / → | Previous / next page |
| Home / End | First / last page |
| ↑ / ↓, Space | Scroll |
| Ctrl +/-, Ctrl+wheel | Zoom |
| Ctrl+0 | Fit width |
| Drag | Select text |
| Double-click / triple-click | Select a word / line |
| Shift+click | Extend the selection |
| Shift + ←/→/↑/↓/Home/End | Extend the selection by character, line, or to the line's start or end |
| Ctrl+A | Select all text on the page |
| Ctrl+C | Copy the selection |
| Esc | Clear the selection |
| Double-click a link | Follow a link within the document |
| Ctrl+click a link | Open a web link in the browser (also follows links within the document) |
| Alt+←, mouse back button | Go back after following a link |
| Drag a thumbnail | Move the page |
| Hover between thumbnails, click "+" | Insert the pages of another PDF there |
| Ctrl+Z | Undo the last text edit, page move or insertion |
| Ctrl+Shift+Z, Ctrl+Y | Redo what was undone |
| ✂ Crop, then drag on a page | Choose the area to keep; the panel crops this page, all pages, or listed pages ("1, 3-5") |
| ⬛ Redact, then drag on a page | Mark an area to redact (click a mark to unmark it); the panel can mark every occurrence of some words, and "Redact" applies the marks |
| Ctrl+E | Edit mode on/off: click text to edit it, or an empty spot to add text |
| Esc (while editing text) | Cancel the edit; clicking outside the text applies it |
| Click a form field | Fill it in (Tab moves to the next field) |
| Ctrl+S | Save the changes (page order, inserted pages, form values) |
| Ctrl+Shift+S | Save as a new file |

As a library:

```rust
use k_pdf::{Document, Renderer, TextExtractor};

let doc = Document::open("file.pdf")?;
let pages = doc.pages()?;

let mut text = TextExtractor::new(&doc);
println!("{}", text.extract_page(&pages[0])?);

let mut renderer = Renderer::new(&doc);          // feature "render"
let image = renderer.render_page(&pages[0], 2.0)?; // RGBA, 2 px per point
```

The default build includes compression, which compiles two C libraries (libdeflate and MozJPEG) from source, so a C compiler is needed (nasm is not). For a reader-only build without the rendering and compression dependencies: `cargo build --no-default-features`; for rendering without compression: `cargo build --no-default-features --features render`.

### Code layout

| File | Purpose |
|---|---|
| `src/object.rs` | PDF object model (`Object`, `Dictionary`, `Stream`, `ObjRef`) |
| `src/parser.rs` | Lenient tokenizer and object parser |
| `src/xref.rs` | Cross-reference loading and damaged-file reconstruction |
| `src/filters.rs` | Stream decoding filters |
| `src/ccitt.rs` | CCITT Group 3/4 fax decoder |
| `src/document.rs` | `Document`: lazy object access, pages, metadata |
| `src/encoding.rs` | Text string decoding, base encodings, glyph names |
| `src/font.rs` | Font encodings, ToUnicode CMaps and widths (shared by text and rendering) |
| `src/std14.rs` | Glyph widths of the standard 14 fonts (generated from Adobe AFM files) |
| `src/edit/mod.rs` | Text editing: paragraphs, styled layout, applying edits |
| `src/edit/content.rs` | Content operations with byte positions; glyph removal |
| `src/edit/fonts.rs` | Fonts for new text: page fonts, system fonts, standard fonts |
| `src/edit/subset.rs` | TrueType subsetting for embedded fonts |
| `src/compress/mod.rs` | Lossless compression: re-encoding streams, merging duplicates, Brotli marking |
| `src/compress/jpeg.rs` | Lossless JPEG optimization (MozJPEG), checked pixel for pixel |
| `src/edit/redact.rs` | Redaction: removing text, image pixels, shapes and annotations in areas; finding phrases |
| `src/forms.rs` | Form fields: reading, filling, appearance streams |
| `src/writer.rs` | Saving documents (pages rearranged, or merged from several documents) |
| `src/text.rs` | Content-stream interpreter for text extraction |
| `src/links.rs` | Link annotations, actions and destinations |
| `src/layout.rs` | Reading order: splits lines into columns and blocks, moves line numbers aside |
| `src/render/mod.rs` | Content-stream interpreter for rendering (graphics state, paths, text, XObjects, annotations) |
| `src/render/color.rs` | Color spaces → sRGB |
| `src/render/function.rs` | PDF functions, including the PostScript calculator |
| `src/render/shading.rs` | Axial and radial shadings |
| `src/render/image.rs` | Image decoding to on-screen size |
| `src/render/glyphs.rs` | Glyph outlines from embedded fonts; system font fallback |
| `src/render/type1.rs` | Type 1 font parser and charstring interpreter |
| `src/main.rs` | Command-line tool |
| `viewer/src/main.rs` | Desktop viewer UI |
| `viewer/src/worker.rs` | Viewer's background render threads (shared job queue) |
| `viewer/src/selection.rs` | Text selection: hit-testing, word/line ranges, caret movement |
| `viewer/src/crop.rs` | Crop mode: choosing the area, crop panel |
| `viewer/src/compress.rs` | The Compress dialog and the Brotli warning |
| `viewer/src/redact.rs` | Redact mode: marking areas, redact panel |
| `viewer/src/editor.rs` | Edit mode: in-place rich-text editor and formatting toolbar |
| `viewer/src/form.rs` | Editable form fields drawn over the page |
| `viewer/src/sidebar.rs` | Page thumbnail sidebar |
| `viewer/src/links.rs` | Link hit-testing, tooltips, which addresses may be opened |
| `viewer/src/titlebar.rs` | Custom title bar: window buttons, dragging, edge resizing |
| `packaging/linux/` | Launcher (`k-pdf.desktop`), icon, `install.sh`, `package.sh` |
| `viewer/assets/icon-128.rgba` | Window icon (the SVG icon rendered at 128 px, raw RGBA) |
| `viewer/src/theme.rs` | Dark and light palettes, styling, the system UI font |
| `viewer/src/icons.rs` | Vector toolbar icons and buttons |

### Testing
`cargo test --workspace` runs unit tests and integration tests that build PDFs byte by byte (`tests/reader.rs`, `tests/render.rs`).

The renderer was also checked against all 4,860 PDFs on the development machine. Every page 1 renders without error. In random samples of 150 pages, about 95% match MuPDF's output to within 0.5% of pixels.

Compression was checked on the 19 PDFs in Downloads and on 300 random PDFs from the development machine. Each file was compressed, and the copy was compared with the original:
- every page rendered by MuPDF matched pixel for pixel;
- the text, form fields and document info were identical.

Every file passed in both modes. The default mode made the 300 files 10.1% smaller (16.5% for the files in Downloads, where photos and repeated form parts are common); with Brotli, 22.7%.

### Known limitations (planned for later phases)
- **Encrypted PDFs:** detected, but not yet decrypted. Structure and page count work; text, metadata and page rendering return `Error::Encrypted`, and they can't be saved.
- **Cropping** only hides the rest of the page (as every PDF tool does); the hidden content is still in the file. Pages inserted from other PDFs can't be cropped until the file is saved and reopened.
- **Redaction** covers what the page itself draws. Some things are not changed:
  - document metadata (title, author, XMP), bookmarks, and alternate text in the structure tree;
  - the glyphs of redacted letters, which remain in embedded fonts;
  - shapes crossing an area's edge (they stay in the file, covered by the box);
  - patterns and shadings.

  Inline images and images that can't be decoded (JPX, JBIG2) are removed whole when an area touches them. A redacted copy of a JPEG image is saved losslessly, which can make the file larger. Pages inserted from other PDFs can't be redacted until the file is saved and reopened.
- **Text editing:** edited text has a fixed position on the page, so text that gets longer can run into what's below it (other content doesn't move). Text inside form XObjects, rotated or invisible text, and Type 3 fonts aren't editable. Fonts with CFF outlines (most `.otf` files) are embedded whole rather than subset, which makes files bigger. The on-screen editor shows text in the closest installed font; the saved page uses the real one. Small caps and ligatures become plain letters when edited.
- **Forms:** JavaScript is not run, so calculated fields, formatting, validation and "gray hint" placeholder text don't work. Signature fields can't be signed, and rich text values are saved as plain text. Thumbnails show the values as last saved. Pages of inserted documents keep their fields as they were, without them being editable.
- **Merging** keeps the first document's metadata, bookmarks and form fields; those of inserted documents are not carried over (their pages, with links and annotations, are). Encrypted PDFs can't be inserted.
- **Compression** never loses anything, so its gains are modest on files that are already efficient. It does nothing for JPEG 2000, JBIG2 or CCITT images, which it copies as they are. It also doesn't subset fonts (unused glyphs are data too). Brotli files can't be opened by many readers, including Poppler-based ones (Evince, Okular, pdftotext), older Acrobat and Foxit, and many phone apps and printers. Brotli files are also no longer valid PDF/A. JPEG XL, which the PDF Association also plans to add, has no published specification for PDFs yet, so it isn't offered.
- **Saving** rewrites the whole file, so digital signatures no longer validate. (Links and bookmarks keep pointing to the same page wherever it moved.)
- **Images:** JPEG 2000 (JPX) and JBIG2 images are not decoded and are shown as gray boxes.
- **Graphics:** function-based and mesh shadings (types 1, 4–7) are drawn as a single average color. Knockout groups are not supported. ICC profiles are not applied; colors use the device-space formulas.
- **Form fields** with no appearance stream show only their background and border.
- **Text selection** works within one page at a time.
- **Links** to other PDF files and links that launch programs are not followed. Only `http`, `https`, `ftp` and `mailto` addresses are opened.
- **Text:** CJK fonts with no `/ToUnicode` map need Adobe CID→Unicode tables to extract their text, though they render fine. Right-to-left text comes out in drawing order. Plain-text extraction (`k-pdf text`) keeps the drawing order; only the viewer's selection uses the column analysis, and tables are read column by column.
- Simple fonts with no `/ToUnicode` and only a built-in encoding inside the embedded font program

### License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

