# k-pdf — technical documentation

How k-pdf is built: the library, the command-line tool, the application's internals, building, releasing, testing and known limitations. For what k-pdf is and how to install it, see the [README](README.md).

k-pdf is a small, low-memory PDF library, CLI and desktop viewer written in Rust.

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
    - the editing tools (Edit text, Crop, Rotate, Redact, Comment) as one group;
    - the file name in the middle, with a dot for unsaved changes;
    - a "⋯" menu (Save as, Print, Compress, Document info, Appearance) and round window buttons on the right. The menu stays open while the theme or dark pages are changed, so the result can be seen and changed again.

    Drag it to move the window, double-click it to maximize, and resize from any edge. In narrow windows the buttons show icons only, and then Undo and Redo move into the menu.
  - Dark pages for reading (moon button, "⋯" menu, Ctrl+Shift+D). Rendered pages are recolored in OKLab:
    - lightness is turned around, white paper to a soft dark gray and black text to a soft light gray;
    - hues are kept and chroma softened slightly, so diagrams stay readable;
    - photos are left as they are: opaque images with little white background, as opposed to drawings, charts, scans and logos.

    Form fields and the text editor follow the same colors. A color cache makes it cost a few milliseconds per page. `Renderer::set_dark_pages` and `dark_page_color` expose it in the library, and `k-pdf-cli render --dark` in the CLI.
  - Page navigation and zoom float at the bottom of the page view. Two buttons next to the zoom switch Fit width and Fit page on or off (fitting follows window and sidebar resizes until you zoom by hand). Click the zoom percentage for those and fixed sizes.
  - A page sidebar with thumbnails of every page: it follows the main view, highlights the current page, and clicking a thumbnail goes to that page. It can be resized or hidden (sidebar button).
  - Sections in the sidebar: the document's outline (bookmarks) as headings with their subheadings indented under them and their page numbers. Headings start unfolded to their subheadings, deeper levels as the file asks; arrows fold them. The section being read (a quarter of the way down the view) is highlighted and kept in view, or the heading it is folded under. Documents of 20 pages or more that have an outline open on the sections, others on the thumbnails; the buttons at the top of the sidebar (or Ctrl+Shift+B) switch, and each view keeps its own width.
  - Comments: selecting text shows a bar over it. A color highlights the text (Ctrl+Shift+H uses the last color); "Comment" highlights it and opens a comment box (on text that is already highlighted, it opens that highlight's comment). The Comment tool in the toolbar adds a note (a speech-bubble icon) where the page is clicked next, and opens its comment box. Clicking a highlight or note opens its box to read, change or delete the comment; hovering shows it. They are standard PDF annotations (`/Highlight` with `/QuadPoints`, `/Text` notes), with the author (login name), dates and an appearance stream, so other readers show them too. Comments made by other programs (underlines, strike-outs, shapes, free text…) are listed, and can be changed and deleted. A whole visit to a comment box is one undo step; a new note left empty is removed.
  - Comments in the sidebar: the third view lists every comment, page by page, with its color, author, date, the highlighted text and the comment. Clicking one goes to it and opens it.
  - Password-protected files: when a file needs a password (`Error::PasswordNeeded` from the workers' `Document::open`), a dialog asks for it: a padlock, the file's name, a masked field with an eye button to show it, Open and Cancel. Enter opens; a wrong password shakes the field, outlines it in red and says so, without closing the dialog. The password that opened a file is kept in memory for the session (`viewer/src/password.rs`), because the workers, the editor, saving, printing and compressing each open the file themselves; they all go through `password::open`. Cancelling goes back to the file that was open before. Inserting a protected PDF asks the same way ("to insert its pages"). Saving keeps the protection (same passwords, permissions and encryption); printing sends an unprotected temporary copy to the printer; files made from images and PDFs in the image editor are new documents and aren't protected. The status bar shows a padlock and "Protected" (the method on hover), and Document info a Protection row. Files whose user password is empty (only the owner's permissions are set) open without asking.
  - Reading position: a file opens where reading left off: the page and the spot on it at the top of the view (in display points, so it holds at any zoom or window size). It is remembered when another file is opened and when the app closes, in a small text file in the user's state folder (`$XDG_STATE_HOME/k-pdf/positions`, by default `~/.local/state/k-pdf/positions`; `~/Library/Application Support/k-pdf/positions`; `%LOCALAPPDATA%\k-pdf\positions`): one line per file, `page<TAB>spot<TAB>path`, most recent first, at most 500 files, replaced whole through a temporary file. It is read once when a file opens and nothing is kept in memory. `--page` and reopening after saving or compressing take precedence. The jump waits for the first layout, after the zoom fits the window, because the offset depends on the zoom (margins and gaps don't scale). Untitled new forms and images aren't remembered. See `viewer/src/positions.rs`.
  - The start screen (no file open) offers two tiles: **Open a PDF or images** (which covers combining, compressing and cleaning up images as before), and **Create a form**, either on a **Blank page** (a new one-page document, Letter in the US and Canada and A4 elsewhere, saved where the user picks: Ctrl+S asks, and the title shows "Untitled form" until then) or **From a PDF…** (picks a PDF and opens it in form mode). Dragging a file over the window shows where to drop it.
  - Editing text (Edit mode, Ctrl+E): click a paragraph to edit it in place, as in a word processor. The text keeps its font, size and color; a toolbar changes the font, size, bold, italic, underline and color of the selection (or of what you type next), and the paragraph's alignment, line spacing and width. Text re-wraps to the paragraph's width. Click an empty spot to add a new text box; delete all of a paragraph's text to remove it.
  - Cropping (Crop mode): drag on a page to choose the area to keep (adjust it with the handles, or drag it to move it), then crop that page, every page, or the pages you list (e.g. "1, 3-5") to the same area. Crops can be removed per page or all at once; Ctrl+Z / Ctrl+Shift+Z undo and redo them.
  - Rotating (Rotate mode): a panel turns the page being read, every page, or the pages you list (e.g. "1, 3-5") a quarter turn left or right, or upside down. Ctrl+R / Ctrl+Shift+R turn the current page right / left without the panel. Each rotation is one undo step.
  - Redacting (Redact mode): with text selected, turning Redact mode on marks the selection (each line's area); otherwise drag over anything on a page to mark it, or type words to mark every place they appear; click a mark to unmark it. "Redact" then removes what is under the marks for good (text, image pixels, shapes, links and notes) and draws black boxes in its place. Ctrl+Z undoes it until the file is saved.
  - Making forms (Form mode, the Form button): a panel picks what clicking or dragging on a page makes: a text box, a paragraph (several lines), a check box or a drop-down list; or Select. Drag to place a field, or click for the usual size. Fields are outlined in blue with their names. Click one to select it: drag it to move it, drag a corner handle to resize it, and change its name, tip (tooltip), text size, several lines, required and the drop-down's choices (separated by commas) in the panel. Delete (or the panel's Delete) removes it, Esc unselects. Fields made by other programs can be moved, changed and deleted too. Each change is one undo step; Done leaves form mode, and the fields can be filled in at once. New fields get a unique name ("Text 1", "Check box 2"…); names must be unique and can't contain periods. The edits are in `k_pdf::edit::fields` (below), recorded in the editor's changes like other edits; the render threads always leave fillable fields' contents out of the open file's pages, so fields made later are drawn once.
  - Signing (the Sign button, a pen, or "Add a signature…" in the ⋯ menu): a dialog with three tabs (it opens on the one used last):
    - **Draw**: a white sheet (white in both themes, like paper) with a line to sign on. Each drag is a stroke, a click a dot; strokes are kept in sheet units (the sheet is 1000 wide) and shown as `k_pdf::scan::drawn_signature` draws them (quadratic curves through the middles of the pointer's steps, round ends and joins). Pen: fine, medium or bold; ink black or blue; Undo stroke and Clear.
    - **Type**: a name field and the name shown in each handwriting font installed (`edit::fonts::handwriting_fonts`, found and read on a thread the first time the dialog opens), as cards on white in two columns; the name in each is made again when the name or the ink changes (40 pixels to the em, by the library). Enter in the field uses it; the signature is made at 160 pixels to the em (`k_pdf::scan::typed_signature`).
    - **Photo or scan**: pick a photo or scan of a signature (an image, or a PDF, whose pages can be stepped through; read on a thread of its own, PDF pages rendered at 3300 pixels as for scanning). It shows with a box to drag around the signature (it starts as the whole picture, so a picture of just the signature needs none; dragging inside a box moves it), and a preview of the lifted signature on a checkerboard, with the ink as written, black or blue (`k_pdf::scan::lift_signature`, see *Signatures* below).

    "Use signature" puts it on the page being read, in the middle of what shows, about two inches wide; drag it to move it (also onto another page), drag a corner to resize it (keeping its shape, within the page), then Place (or Enter) draws it into the page (`k_pdf::edit::place_image`), one undo step; Esc or Cancel drops it. While placing, it is drawn with a texture of white with the signature's opacity, tinted with its ink (through `theme::on_page`, so it looks right on dark pages). The last signature is kept for the session: the Sign button then offers it first ("Use this signature"), or a new one. See `viewer/src/signature.rs`.
  - Filling in forms: text fields (single and multi-line, password, length limits), check boxes, radio buttons and drop-down lists are editable on the page. Fillable fields are marked: a light blue tint (stronger under the pointer and while typing) and a blue border, red for required fields, drawn over any background the field has so every fillable field looks the same; read-only fields aren't marked. The field being typed in gets a deeper tint, a 2-point darker blue border (red if required) and a blue glow around it, even with the marking off. Its text cursor is drawn in the field's text color and blinks in empty fields too; text is inset from the border as in the saved appearance (2 pt, at least 4 screen points so the cursor clears the border). egui ignores `TextEdit::margin` when a frame is given, so the inset is the frame's margin. The status bar says how many fields there are to fill in, and "Highlight fillable fields" in the ⋯ menu (shown when there are some) turns the marking off for the session. Save writes the values into the file, with appearances that every PDF viewer shows.
  - Inserting PDFs: hovering the gap between two thumbnails (or above the first, or below the last) shows a "+" button; click it and pick a PDF to insert its pages there, or images. Images first open in the image editor (see *Images as a document* below; the toolbar shows **Cancel** and **Insert N pages** instead of saving, and the document's tools are hidden): each can be kept as it is or scanned, cropped, turned, reordered, and more added. **Insert** makes the pages into a temporary PDF (removed when k-pdf closes) and inserts it as one undoable step; **Cancel** goes back to the document unchanged. "Insert pages or images…" in the ⋯ menu does the same after the current page.
  - Images as a document (opening or dropping JPEG, PNG, GIF, BMP, TIFF or WebP images, choosing several in the Open dialog, or `k-pdf a.jpg b.png`). PDFs can be added too (Add images, or dropped on the window): each of their pages becomes a page here, rendered at 1600 pixels to show; kept **As is**, it is copied into the saved PDF unchanged (text, fonts and drawings; turning sets its `/Rotate`), and with any other look it is rendered at 3300 pixels and made into a scanned page. A PDF page's corners start on the page's edges. When some pages are kept PDF pages, the made pages are first written to a temporary PDF, then everything is put together with `write_document`. In short: the images are shown like the pages of a PDF, one page each, and saved together as one PDF. The window keeps its layout: the toolbar (Open, Save PDF, undo and redo, Add images, turn left or right, remove), the sidebar with the pages' thumbnails, the page shown large, and page navigation at the bottom. In the sidebar, thumbnails are dragged to reorder pages; hovering one shows a bin to remove it; right-click to move it up or down, remove it, or add images before or after it; the "Add images" tile at the end adds more (dropping images on the window adds them too). The bar above the page sets the page's look: **As is** (the whole image, unchanged), **Cropped** (the page cut out of the photo and flattened, colors as photographed), or a scan: **Color scan**, **Gray scan**, **Black & white**, where the page is also cleaned up (see *Scanning* below). Scan looks add **Corners** (the photo turned like its page, with the page's corners as handles to drag; a magnifier shows the corner up close while dragging, with the neighboring edges drawn in; "Whole image" and "Find page" set them all at once; Esc or Done when finished) and **Straighten** (level slightly slanted text); **Handwriting** (see *Scanning*, step 6) cleans up for handwriting. **Apply to…** gives other pages this page's look, Straighten and Handwriting: all pages, or a list ("1, 3-5"). Choosing a scan look opens the corners only for a photo whose page wasn't found (a PDF's page is the whole sheet). A page whose look is a scan and whose edges weren't found opens on its corners. Every change is one undo step. Ctrl+S asks where to save the first time (next to the first image, named after it), then saves there again; once saved, **Open PDF** in the toolbar opens the saved file in the viewer (asking first if there are changes since); closing or opening another file with unsaved changes asks first. Pages are written one at a time from the full images: kept images are copied in unchanged when they are JPEGs (and need no turning), stored without loss when they come from PNG, GIF, BMP or TIFF (1 bit per pixel when only black and white), unless that is over three times the size of a quality-90 JPEG (a photo saved as PNG), and WebP images become JPEGs. Memory: each page keeps only small textures (480 pixels, the image and its scanned page) for the thumbnails; the three pages looked at last also keep a 1600-pixel copy (to show large and to make previews from), read again from the file when needed. Scanned previews are made in the background, the shown page first. Twenty 7-megapixel photos settle at about 120 MB. See `viewer/src/images.rs`, `src/scan/`.
  - Cleaning up scanned pages (the sparkle button, or "Clean up scanned pages…" in the ⋯ menu): the document's pages, in their current order and with unsaved edits (the editor's `Changes` as an overlay on the open file's pages; pages inserted from other files from those files), open in the image editor **As is**, at the page being read, with the document kept open behind it. Handwriting starts ticked (it applies once a scan look is picked). Adding, removing and reordering are left to the document (the sidebar only selects). The toolbar shows **Cancel** (back, unchanged) and **Apply** (enabled once a page has a scan look or is turned): pages with a scan look are made at 3300 pixels into a temporary PDF, which is opened like an inserted file and its pages put in place of the old ones (`replacing`, one `Undo::Order` step); pages kept As is but turned are rotated in the document (`rotate_pages`, keeping their text). Then it's the document again with every tool; Ctrl+S saves.
  - Deleting pages: hovering a thumbnail shows a bin button that deletes that page; right-clicking a thumbnail offers "Delete page N", "Delete pages…" and inserting a PDF before or after it; "Delete pages…" (also in the ⋯ menu) deletes a list or range ("2, 5-7"), saying how many of how many pages go. At least one page always stays. Deleting is one undo step (Ctrl+Z) until the file is saved; saving writes the remaining pages, and form fields that were only on deleted pages are left out of the file too (see `FieldPruner` in `src/writer.rs`), as are the deleted pages' comments from the comments list.
  - Presenting (the screen button in the toolbar, "Present" in the ⋯ menu, F5 from the first page, Shift+F5 from the current one): the window goes full screen and shows one page at a time on black, rendered at screen resolution (the next ones are rendered ahead). Slides change with a fade by default (or a push, or no transition; 0.45 s, eased). →, Space, Page Down, Enter, a click or the wheel go forward; ←, Backspace, Page Up, a right click or the wheel back; Home / End; a number then Enter jumps to that slide; B / W blank the screen black / white; Esc ends and goes back to the page last shown. The pointer hides when it doesn't move, and the slide number shows while it does. The presenter view, a second window with the app's own title bar (P shows or hides it, closing it ends the show; whether it opens is a setting in the ⋯ menu), shows the current and next slide, the page's comments as speaker notes, a timer with pause and reset, and buttons for moving, the transition and blanking. With more than one screen and the presenter view on, the slides fill a screen other than the main one (the primary one, or the one the viewer was on), and the presenter view opens maximized where the desktop puts it (the screen being used); "Slides on" and "Swap screens" in the presenter view move them, and then the presenter view goes to its chosen screen too (screens listed twice by the system are shown once). With one screen, or the presenter view turned off, the slides fill the current screen and no presenter window opens. See `viewer/src/present.rs`.
  - Printing ("Print…" in the ⋯ menu, Ctrl+P): a dialog with the printer (each printer's own paper sizes, and whether it can print two-sided and in color), copies and collating, the pages (all, the current one, or a list like "1-3, 5"; odd or even page numbers only; reverse order), two-sided (long or short edge), orientation (automatic, portrait, landscape), pages per sheet (1, 2, 4, 6, 9, 16), scaling (fit to paper, actual size, shrink large pages), paper size, color or black and white, and whether highlights and notes are printed. A preview shows each sheet. The pages are written to a temporary PDF with the unsaved changes and form values (and without comments if they are left out, via `edit::annotate::remove_comments`), then sent:
    - Linux and macOS: to CUPS with `lp` (printers from `lpstat`, their options from `lpoptions -l`); CUPS does pages per sheet, scaling, orientation and paper. See `viewer/src/print/cups.rs`.
    - Windows: through GDI. The options go into the printer's DEVMODE, and each sheet is rendered by k-pdf (at up to 300 dpi) and drawn on the printer's page; pages per sheet, scaling and black and white are done by k-pdf. See `viewer/src/print/windows.rs`.
  - Compressing ("Compress…", next to Save as): saves a smaller copy, including unsaved changes, that looks and works exactly the same. JPEG optimization is on by default. Brotli is an option that is off by default; ticking it shows a warning that many PDF readers can't open such files. The dialog then shows how much smaller the copy is, and can open it.
  - Rearranging pages: drag a thumbnail in the sidebar to move the page. Save (Ctrl+S) writes the new order to the file, Save as (Ctrl+Shift+S) to a copy, and Ctrl+Z undoes a move or an insertion, and Ctrl+Shift+Z (or Ctrl+Y) redoes it. Closing the window or opening another file with unsaved changes asks whether to save them.
  - Continuous scrolling, zoom (the floating controls, Ctrl+wheel, Ctrl +/-) and page jumping.
  - Finding words (Ctrl+F, or the magnifier in the toolbar): a bar at the top right of the pages searches the whole document as you type, from the current page on, and shows "3 of 27" (with "+" while pages are still being searched). Every match is highlighted and the current one is stronger; Enter / Shift+Enter, F3 / Shift+F3, Ctrl+G / Ctrl+Shift+G and the arrows go to the next or previous one (scrolling only when it's out of view), Esc closes. Case and line breaks are ignored ("Aa" matches case, "ab|" whole words only); typographic quotes, dashes and ligatures match their plain forms. Pages are searched by the render threads, each page's text matched as it comes (`Job::Search`), so a 4,000-page manual takes about a second and no text is kept in memory.
  - Finding words in images ("Images" in the search bar, on by default): pages that draw images also have the text in them read with text recognition (OCR, see *Reading text in images* below), one page at a time after everything else, and matched allowing a letter in six to be wrong (OCR misreads now and then). The bar says how many pages' images are still being read. Pages without text of their own (scans) are read as soon as they are shown, so their text can be selected, copied and highlighted like any other. Text read from a page is kept until the page changes (an edit, a redaction).
  - Text selection and copying. Selection follows reading order: it stays within a column and skips margin line numbers (as in review copies of papers).
  - Links: double-click follows a link within the document (contents, sections, figures, citations); web links open in the browser only with Ctrl+click. Hovering shows where a link goes, and Alt+← (or the mouse back button) returns.
  - A document info window (file, file size, PDF version, pages, page size, metadata): a separate window (it can be moved outside the viewer), resizable from its edges and corners, with long values wrapped.
  - Several threads render pages in parallel, and the pages just ahead in the scroll direction are prepared in advance.

- **Links:** `page_links` returns a page's links with their rectangles and targets: web addresses, or a page and position in the document (go-to actions, explicit and named destinations, next/previous/first/last page).
- **Rotating:** `edit::rotate_page` turns a page by a multiple of 90° (its `/Rotate`, set on the page itself so an inherited value can't come back); the content isn't touched.
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
- **Pictures on pages (signatures):** `edit::place_image` draws an RGBA picture on a page over a rectangle in display points, upright as the page is shown whatever its rotation. It becomes page content, not an annotation (it prints, isn't a comment, and isn't removed with comments): an image XObject (gray when it has no color) with an 8-bit soft mask for its transparency, interpolated, Flate-compressed; the page's content streams stay as they are, with a `q` stream added before them and a `Q` and the picture after, so state the old content leaves behind can't move it. `scan::lift_signature` lifts a signature off a photo or scan (see *Signatures*); `scan::drawn_signature` draws one from pointer strokes and `scan::typed_signature` writes a name in a font file (both with tiny-skia, so they need the `render` feature); `edit::fonts::handwriting_fonts` lists the handwriting fonts installed and `edit::fonts::family_font` reads one.
- **Password-protected PDFs** (`src/crypt/`): the standard security handler (ISO 32000-2 §7.6.4), revisions 2 to 6: RC4 from 40 to 128 bits, AES-128 (AESV2) and AES-256 (AESV3, including the deprecated revision 5), with the user or the owner password. `Document::open` tries the empty user password (files restricted only by an owner password open as before) and fails with `Error::PasswordNeeded`; `Document::open_with_password` / `from_bytes_with_password` take one and fail with `Error::WrongPassword`. Other security handlers (certificates) give `Error::Encrypted`. The key is worked out once at open; each object's strings and stream data are decrypted as the object is loaded from the file (objects in object streams come decrypted with their stream; cross-reference streams, the encryption dictionary, signature `/Contents`, metadata with `/EncryptMetadata false` and streams with an identity `/Crypt` filter are left alone), so memory use is that of an unprotected file. The writer encrypts each object it writes directly with the same key (object streams as a whole; fresh AES IVs) and writes the encryption dictionary and `/ID` unchanged, so saved files open with the same passwords and keep their permissions; `WriteOptions::decrypt` saves without protection. MD5, SHA-256/384/512, AES and RC4 are small built-in implementations (`src/crypt/hash.rs`, `aes.rs`), with no new dependencies. `Document::encryption` describes the method ("AES-256").
- **Making forms:** `edit::fields::add_field` adds a text field, check box or drop-down list to a page at a rectangle in display points, as one object that is both field and widget, listed in the page's `/Annots` and the form's `/Fields`. The document gets an `/AcroForm` (moved out of the catalog into an object of its own if it was inline) with `/DA` and Helvetica and ZapfDingbats in `/DR`, and `NeedAppearances` is removed because every field gets appearance streams (a border; check boxes `Yes`/`Off` with a ZapfDingbats check). On rotated pages the field gets `/MK /R` so its text is upright. `update_field` changes name, tooltip, required, multi-line, text size (`/DA`) and choices (`/Opt`); `move_widget` moves or resizes a widget; `delete_field` takes the widgets off their pages and the field out of `/Fields` or its parent's `/Kids`. Each makes new appearances. `blank_document` writes a one-page PDF to start from.
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

The application is called **k-pdf**; the command-line tool is **k-pdf-cli**. Releases on GitHub have a download for each system:

**Linux** — `k-pdf-<version>-linux-x86_64.tar.gz`: unpack it and run

```sh
./install.sh             # into ~/.local: k-pdf appears in the applications menu
./install.sh --default   # …and opens PDF files by default
./install.sh --uninstall
sudo PREFIX=/usr/local ./install.sh   # for all users
```

Built on Ubuntu 22.04, it runs on Linux systems from 2022 on.

**Windows** — `k-pdf-<version>-windows-x86_64.zip`: unpack it anywhere and run `k-pdf.exe`. The download isn't code-signed, so Windows SmartScreen may warn the first time: choose *More info → Run anyway*.

**macOS** — `k-pdf-<version>-macos-universal.dmg`, for both Apple Silicon and Intel Macs (macOS 11 or later). Open it and drag k-pdf into Applications. The app isn't notarized by Apple, so the first time macOS refuses to open it: either open *System Settings → Privacy & Security* and click *Open Anyway*, or run `xattr -dr com.apple.quarantine /Applications/k-pdf.app`. The command-line tool is inside the app: `/Applications/k-pdf.app/Contents/MacOS/k-pdf-cli`. On macOS, open files from k-pdf's Open button or by dropping them on its window (double-clicking a PDF in Finder doesn't open it in k-pdf yet).

**From source** (any system with Rust and a C compiler): `cargo build --release --workspace`; the programs are in `target/release/`. On Linux, `packaging/linux/install.sh` installs them (same options as above), and `--link` links to the build instead of copying it, so rebuilds show up at once.

**Releasing:** pushing a version tag (`git tag v0.1.0 && git push origin v0.1.0`) makes GitHub Actions (`.github/workflows/release.yml`) run the tests, build all three downloads, and publish them as a release. Running the workflow by hand only builds them, as downloadable artifacts. Locally:
- `packaging/linux/package.sh` builds the Linux archive into `dist/`;
- `packaging/windows/package.sh` builds the Windows one (in Git Bash);
- `packaging/macos/bundle.sh` builds the macOS one.

### Usage

```sh
cargo build --release --workspace
./target/release/k-pdf [file.pdf] [--page N]            # the application

./target/release/k-pdf-cli info   file.pdf
./target/release/k-pdf-cli text   file.pdf [--page N]
./target/release/k-pdf-cli object file.pdf <num> [gen] [--decode]
./target/release/k-pdf-cli render file.pdf <page> out.png [--scale S] [--dark]
./target/release/k-pdf-cli reorder in.pdf out.pdf 3,1,2      # or ranges: 5-10,1-4
./target/release/k-pdf-cli delete  in.pdf out.pdf 2,5-7      # a copy without those pages
./target/release/k-pdf-cli merge   out.pdf a.pdf b.pdf ...
./target/release/k-pdf-cli crop    in.pdf out.pdf 40 30 572 400 [1,3-5]   # left top right bottom
./target/release/k-pdf-cli rotate  in.pdf out.pdf 90 [1,3-5]               # clockwise; -90 or 270 for left
./target/release/k-pdf-cli redact  in.pdf out.pdf 1 72 90 300 120         # page left top right bottom
./target/release/k-pdf-cli redact  in.pdf out.pdf --text "Jane Doe" [1,3-5]
./target/release/k-pdf-cli compress in.pdf out.pdf [--no-jpeg] [--brotli]   # lossless; --brotli: see below
./target/release/k-pdf-cli text    locked.pdf --password secret   # --password goes with any command
./target/release/k-pdf-cli decrypt locked.pdf open.pdf --password secret   # a copy without the password
./target/release/k-pdf-cli blocks  file.pdf 1                 # list a page's text blocks
./target/release/k-pdf-cli edit    in.pdf out.pdf 1 4 "New text for block 4"
./target/release/k-pdf-cli form    form.pdf                   # list the fields
./target/release/k-pdf-cli outline book.pdf                   # list the sections with their pages
./target/release/k-pdf-cli comments paper.pdf                  # list the highlights and notes
./target/release/k-pdf-cli highlight in.pdf out.pdf "key term" --comment "Define this"
./target/release/k-pdf-cli note in.pdf out.pdf 1 300 150 "A note at (300, 150) on page 1"
./target/release/k-pdf-cli fill    form.pdf out.pdf "Name=Ada Lovelace" "Agree=on"
./target/release/k-pdf-cli blank   new.pdf [letter|a4|<width>x<height>]   # one blank page, to start a form
./target/release/k-pdf-cli add-field in.pdf out.pdf text "Full name" 1 72 100 400 124 [--multiline] [--required] [--size 11] [--tooltip "…"]
./target/release/k-pdf-cli add-field in.pdf out.pdf check Agree 1 72 140 88 156
./target/release/k-pdf-cli add-field in.pdf out.pdf dropdown Color 1 72 180 250 202 --options "Red,Green,Blue"
./target/release/k-pdf-cli sign    in.pdf out.pdf 1 72 640 150 --from photo.jpg [--area x0,y0,x1,y1] [--ink original|black|blue]
./target/release/k-pdf-cli sign    in.pdf out.pdf 1 72 640 150 --from scan.pdf --source-page 2 --area 400,2600,1500,2900
./target/release/k-pdf-cli sign    in.pdf out.pdf 1 72 640 150 --text "Jane Doe" [--font Z003] [--ink black|blue]
./target/release/k-pdf-cli sign --fonts      # the handwriting fonts --text can use
./target/release/k-pdf-cli search  file.pdf "words" [--case] [--words] [--no-images]   # each match with its page and context
./target/release/k-pdf-cli ocr     scanned.pdf [--page N]                          # the text read in the images
./target/release/k-pdf-cli scan    out.pdf page1.jpg page2.png [--mode color|gray|bw|original]
./target/release/k-pdf-cli scan    out.pdf photo.jpg --corners 420,380,2010,300,2150,2860,300,2950
./target/release/k-pdf-cli scan    out.pdf scan.png --whole --no-straighten
./target/release/k-pdf-cli scan    clean.pdf notes.pdf --handwriting --mode bw   # clean up a scanned PDF, every page
./target/release/k-pdf-cli images  out.pdf a.jpg b.png c.tif d.webp   # one page per image, as it is
```

`k-pdf a.jpg b.png` (or images dropped on the window) shows the images as the pages of a new PDF.

Viewer keys:

| Keys | Action |
|---|---|
| Ctrl+O | Open a file |
| PageUp / PageDown, ← / → | Previous / next page |
| Home / End | First / last page |
| ↑ / ↓, Space | Scroll |
| Ctrl +/-, Ctrl+wheel | Zoom |
| Ctrl+0 | Fit width |
| Ctrl+9 | Fit page |
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
| Ctrl+Shift+B | Switch the sidebar between pages, sections and comments |
| Ctrl+Shift+H | Highlight the selected text (in the last color used) |
| Ctrl+Enter, Esc | Close the comment box (what was typed is kept) |
| Drag a thumbnail | Move the page |
| Hover between thumbnails, click "+" | Insert the pages of another PDF there, or images as pages |
| Hover a thumbnail, click the bin | Delete that page |
| Right-click a thumbnail | Delete it, delete a list of pages, or insert a PDF before or after it |
| Ctrl+Z | Undo the last text edit, page move or insertion |
| Ctrl+Shift+Z, Ctrl+Y | Redo what was undone |
| Ctrl+R / Ctrl+Shift+R | Rotate the current page right / left |
| Rotate | Panel: rotate this page, all pages, or listed pages ("1, 3-5") left, right or 180° |
| ✂ Crop, then drag on a page | Choose the area to keep; the panel crops this page, all pages, or listed pages ("1, 3-5") |
| ⬛ Redact, then drag on a page | Mark an area to redact (click a mark to unmark it); the panel can mark every occurrence of some words, and "Redact" applies the marks |
| Ctrl+E | Edit mode on/off: click text to edit it, or an empty spot to add text |
| Esc (while editing text) | Cancel the edit; clicking outside the text applies it |
| Click a form field | Fill it in (Tab moves to the next field) |
| Form, then drag on a page | Place a field (click: the usual size); click a field to select it, drag it or its corners to move or resize it |
| Sign, then drag the signature | Move it (also onto another page); drag a corner to resize it |
| Enter / Esc (placing a signature) | Place it / drop it |
| Enter (typing a signature's name) | Use it |
| Delete / Esc (Form mode) | Delete the selected field / unselect it |
| Ctrl+S | Save the changes (page order, inserted pages, form values) |
| Ctrl+Shift+S | Save as a new file |
| Ctrl+F | Find words (the search bar; again: back to its field) |
| Enter / Shift+Enter (in the search bar), F3 / Shift+F3, Ctrl+G / Ctrl+Shift+G | Next / previous match |
| Esc (in the search bar) | Close it |
| Ctrl+P | Print |
| F5 / Shift+F5 | Present from the first page / from the current page |
| While presenting | → Space PgDn Enter click: next · ← Backspace PgUp right-click: back · Home / End · number + Enter: go to slide · B / W: black / white screen · P: presenter view · Esc: end |

With images open:

| Keys | Action |
|---|---|
| ← / →, PageUp / PageDown, ↑ / ↓ | Previous / next page |
| Home / End | First / last page |
| Ctrl+R / Ctrl+Shift+R | Turn the page right / left |
| Delete | Remove the page |
| Drag a corner (Corners) | Put the page's corner there (a magnifier shows it up close) |
| Esc | Stop placing corners |
| Drag a thumbnail | Move the page |
| Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y | Undo, redo |
| Ctrl+S, Ctrl+Shift+S | Save the pages as one PDF; save it elsewhere |

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

Scanning a photo:

```rust
use k_pdf::scan::{self, Image, Mode, ScanOptions};

let photo = Image::open("page.jpg")?;                     // JPEG, PNG, GIF, BMP, TIFF or WebP, turned upright
let quad = scan::find_page(&photo).unwrap_or_else(|| scan::whole(&photo));
let page = scan::scan(&photo, &ScanOptions { mode: Mode::BlackWhite, ..ScanOptions::new(quad) });
let mut pdf = scan::PdfWriter::new(std::fs::File::create("scan.pdf")?)?;
pdf.add_page(&page)?;
pdf.add_image(&std::fs::read("drawing.png")?, 0)?;        // an image as it is (0 quarter turns)
pdf.finish()?;
```

### Toolbar order

Left to right: Open, Save; Undo, Redo; the editing tools in a group (Edit text, Crop, Rotate, Redact, Form, Comment, Sign); viewing aids that don't change the document (Search, Sidebar, Dark pages); then the tools that open a view of their own (Clean up scanned pages, Present, which takes over the screen and so comes last). Less used things are in the ⋯ menu; when the window is narrow, the buttons lose their labels, then undo and redo move into the menu.

### File dialogs

The Open, Save, Insert and Add images dialogs are shown from a thread of their own (`viewer/src/dialog.rs`), and the viewer acts on the answer when it arrives. On Linux, rfd shows the desktop's file chooser through the XDG portal and waits for it on the thread that asked: asked from the UI thread, the window stopped drawing and answering the desktop while the chooser was open, and after a few seconds (browsing a folder of large photos, say) GNOME offered to force quit k-pdf. One dialog is open at a time; asking for another meanwhile does nothing. On macOS rfd hands the panel to the main thread itself.

### Reading text in images

`src/ocr.rs` (feature `ocr`, on by default) reads text in images with [ocrs](https://github.com/robertknight/ocrs) 0.12 and the rten runtime (pure Rust; later versions need Rust 1.94). Two neural networks, `text-detection.onnx` (finds words) and `text-recognition.onnx` (reads lines), about 12 MB, read Latin script. They are not built into the program:

- release packages carry them in `models/` next to the program (Windows, Linux; `install.sh` copies them to `$PREFIX/share/k-pdf/models`) or in `Contents/Resources/models` (macOS);
- `packaging/fetch-ocr-models.sh [dir]` downloads them (checked against fixed SHA-256 sums), by default into the user's data folder: `~/.local/share/k-pdf/models`, `~/Library/Application Support/k-pdf/models` or `%LOCALAPPDATA%\k-pdf\models`;
- `K_PDF_OCR_MODELS` points at another folder.

Without them everything else works; the search bar's "Images" switch is off and says why.

How a page is read (`read_page_images`): pages that draw no images are skipped without rendering (`page_has_images` looks at the resources and for inline images). Otherwise the page is rendered at 180 dpi; the renderer reports where images were drawn (`RenderedPage::images`), and only those areas (joined where they overlap) are read. Words lying on the page's own text are dropped, so scans that already have a text layer, or captions over pictures, aren't found twice. The result is a list of `TextChar`s in display points, like extracted text, so searching and selecting treat it the same way.

How an image is read (`Ocr::read`):

1. **Finding words** (`find_words`). The word-finding network takes 600×800 pixels; ocrs squeezes bigger images into that, which shrank a 180 dpi page's print to under half its width, so small print ran together. Instead the image is cut into overlapping tiles of that size, read at full size, and the network's verdicts are put together into one mask; words are its connected parts (as in ocrs). Blank tiles (margins: fewer than 24 pixels far from the tile's middling brightness) are skipped, and "words" with hardly any contrast are dropped (`contrasting`), since at full size the network now and then takes a scan's speckles for print.
2. **Levelling** (`text_angle`, `rotate`). Whether text runs across or down is told by the words' long sides; the slant is measured along whole lines (a straight line through each line's word centres, the median over lines). Sideways text, or text off level by more than 0.4° (up to 15°), is turned level, and the word outlines are turned with it rather than found again. Lines read on a slant take in parts of the lines above and below.
3. **Up or down** (`upside_down`). Latin print has much more ink above its small letters' middle band (ascenders, capitals, digits) than below it (descenders): about 3 to 1 on the test pages. Over at least five words, if the ink below wins by half again, the image is given half a turn.
4. **Reading** the lines (ocrs's layout and recognition, eight lines at a time), then turning the characters' rectangles back to the original image.
5. **Look-alike letters** (`same_case`): c, o, s, u, v, w, x and z differ only in size between capitals and small letters, and are sometimes read in the wrong case ("PGo"). They take the case of the rest of their word when it is all one case, except a small s after capitals ("APIs").

Accuracy was measured on 13 synthetic 300 dpi pages with known text, made with PIL (clean print; 8 pt and 6 pt; 150 and 100 dpi scans with noise, blur and JPEG; two columns; gray on gray; white on a dark box; sideways; upside down; 3° crooked; monospace; 18 pt). Words read correctly, before → after: clean 95.6% → 97.2%, 8 pt 89.8% → 95.0%, 6 pt 86.3% → 91.1%, 150 dpi scan 92.2% → 94.4%, crooked 70.0% → 90.9%, sideways 0.6% → 96.9%, upside down 2.2% → 95.9%; the mean went from 75.5% to 94.4%, and character errors from 15.3% to 0.9%. Beam-search decoding (ocrs's `DecodeMethod::BeamSearch`) made no difference and was slower; reading tiles shrunk by 0.75 or 0.6 was worse.

Speed and memory: about 0.7–1.1 s a page (with four threads about 1.25 s, a quarter slower than reading two squeezed bands, for the full-size tiles). Only one tile goes through the word-finding network at a time, and lines are read eight at a time, so reading a page needs about 230 MB at its peak with the viewer's settings (it was 330 MB before tiling). The viewer runs the networks on at most four threads (`RTEN_NUM_THREADS`), limits glibc to two memory arenas, and gives freed memory back to the system after each page (`malloc_trim`), so it peaks at about 350–380 MB while reading a scanned document (the same as before tiling) and goes back to about 100 MB.

### Signatures

How `scan::lift_signature` (`src/scan/signature.rs`) lifts a signature out of the box drawn around it (shrunk first to at most 1600 pixels on its longer side):

1. **The paper**: on a grid of cells a sixth of the box's shorter side, each cell's brightest fifth is averaged, and these colors are spread bilinearly between the cells' middles. Shadows and light falling off across a photo are then paper, not ink; strokes are thin, so cells they cross still have plenty of paper.
2. **Opacity**: each pixel's darkness against the paper there. The paper's grain is measured (the median and median absolute deviation of all the darkness values: the paper is most of the box), and opacity goes smoothly (smoothstep) from 0 at five times the grain above the paper (at least 0.06) to 1 at 70% of the strokes' median darkness. Strokes keep smooth edges and faint parts.
3. **Specks**: pieces of ink (touching pixels at least half opaque) smaller than 6 pixels or 1/400 of the largest piece are cleared, as are faint pixels not within two pixels of a piece kept. The dot of an i stays.
4. **Color and trimming**: one color for the whole signature: "as written" is the average of the solid ink divided by the paper's color there (which takes out a tinted light), deepened by 20% (a photo's ink is lighter than the pen's); or black (24, 24, 28) or blue (22, 48, 150). The picture is trimmed to the ink with 2 pixels to spare.

Checked on a synthetic phone photo (a script signature in blue on paper with light falling off by a quarter towards one corner, grain, blur and JPEG at quality 82): the signature came out with the paper fully transparent and the ink a clean blue, and drawn into a form copy MuPDF and k-pdf show it the same.

**Drawn signatures** (`scan::drawn_signature`): the strokes' extent (plus half the pen) is drawn up to 4 times finer than the units it was drawn in, at most 1600 pixels on the longer side, with tiny-skia: each stroke as quadratic curves through the middles of its steps (so the pointer's sampling doesn't show as corners), round caps and joins; a stroke of one point is a dot. The coverage becomes the opacity, in one ink color. **Typed signatures** (`scan::typed_signature`): each letter's outline from the font (ttf-parser), spaced by its advance and the `kern` table's pairs (there is no shaping, so a script font's joining alternates aren't used), filled with tiny-skia, and trimmed to the ink, which a script's swashes can take past the advances. **Handwriting fonts** (`edit::fonts::handwriting_fonts`): known script and hand-lettered families from Windows (Segoe Script, Lucida Handwriting, …), macOS (Snell Roundhand, Apple Chancery, …) and Linux packages and Google fonts (Z003, Great Vibes, Dancing Script, …), in that order, then other families named "script", "handwriting", "chancery" or "calligraphy", at most 8; copies of Z003 are dropped, and with fewer than 3, the system's italic serif and sans fill in. On this machine: Z003, QTMerryScript, QTHandwriting, QTChanceryType and Comic Neue.

### Scanning

How `src/scan/` turns a photo into a scanned page:

1. **Reading** (`mod.rs`, `png.rs`, `formats.rs`): JPEG with jpeg-decoder, turned upright with the Exif orientation tag (phones often store photos sideways); PNG with a small reader of its own (every color type and bit depth, interlaced or not); GIF (the first frame, interlaced or not), BMP (1 to 32 bits, bit fields, RLE) and TIFF (strips or tiles; uncompressed, PackBits, LZW, Deflate, JPEG and fax G3/G4, with the PDF filters' LZW and CCITT decoders; gray, palette, RGB, CMYK; the first page only) with readers of their own; WebP with the pure-Rust image-webp crate. Transparency goes on white. Images over 120 megapixels are refused. The format is told by the first bytes, not the file name.
2. **Finding the page** (`detect.rs`), on a 480-pixel copy, in "bright minus colorful" (paper is light and colorless):
   - regions: Otsu's threshold, the largest region with its holes filled, its convex hull, and the largest four-cornered shape on the hull;
   - edges: a Hough transform on gradient edges (each edge pixel votes only for lines along it), and every pair of near-horizontal and near-vertical lines (the photo's borders included, for pages cut off by the frame).

   Each candidate is scored by how much of its outline runs along an edge with the gradient across it (stretches on the photo's border count neither way, but may be at most half). The best one, if at least half its outline is on edges, is refined by fitting each side to the edge pixels along it. Regions alone are fooled by strong shadows; edges alone by straight lines in the background; together they found the corners of the test photos to within a pixel or two.
3. **Flattening** (`clean.rs`): a projective map (Heckbert's square-to-quad) with bilinear sampling; the photo is first shrunk by whole blocks when the page is much smaller than it. The page's size is its longer measured sides, snapped to A4, Letter or Legal proportions when within 5% (perspective distorts them a little) unless the corners already form a rectangle; at most 3300 pixels long (A4 at 280 dpi).
4. **Evening out the light**: the paper's color is measured on a 40×40 grid (the brightest fifth of each cell), cells much darker than the paper elsewhere (pictures) take their neighbors' color, the grid is smoothed (extended linearly past its edges, so light falling off at an edge is kept), and every pixel is divided by the paper color at its place. This removes shadows and color casts at once.
5. **Straightening**: the slope of text lines is found by projection (the angle, within ±8°, at which dark pixels pile up into the fewest rows), and the page is turned back if it's at least 0.1° and clearly better than level.
6. **The look**: Color and Grayscale stretch contrast (paper texture to white, the darkest ink to black, middle tones a little darker) and sharpen (unsharp mask); Black & white thresholds with Otsu's method; Original photo keeps the flattened photo as it was. With **Handwriting** (`ScanOptions::handwriting`, `--handwriting`), for pen and pencil, whose strokes vary from dark to barely there:
   - the paper's level and noise are measured (median and MAD of the bright pixels), and the white point follows them instead of a fixed 232;
   - the strong ink is the darker half of what is clearly ink; around each place the darkest ink nearby (block minima 1/250 of the page, smoothed) is brought towards it, up to four times as dark, so faint strokes come out like the others; places with no more contrast than the paper's noise are left alone;
   - ruled lines are found as lines of marks covering over a third of the page's width (or height, for a margin), followed along their measured slope (up to 1°), and kept as light as they were (in black and white they are left out);
   - colors keep their hue (a blue pen gets deeper blue);
   - black and white uses hysteresis: clearly dark pixels are ink, and lighter ones too where they join them, so faint strokes stay whole without noise specks; large dark areas along the edges (a scanner's lid) are dropped;
   - straightening measures faint pages with the darkest 4% of pixels as ink (pencil is lighter than the fixed 110 used for print).

   Printed text would come out bolder, so it is off by default (and ticked to begin with in "Clean up scanned pages"). Without it the output is unchanged (checked pixel for pixel against the earlier scans). On a synthetic notebook page (blue pen and pencil at varying pressure on ruled paper, with uneven light, a 1.6° slant, blur, noise and JPEG artefacts) the faint lines, nearly invisible in the scan, came out dark and whole, and the ruled lines stayed light; a page takes about 1.5 s at 3300 pixels. PDFs are scanned by rendering each page at 3300 pixels (`scan::pdf_page_image`); CCITT-coded scans render like MuPDF's.
7. **The PDF** (`PdfWriter`, one page at a time, so long scans don't need every page in memory): JPEG at quality 85 (MozJPEG; color pages without color are stored as gray), or 1 bit per pixel with Flate for black and white (about 60 KB a page). Pages are A4, Letter or Legal when the proportions match, otherwise fitted on A4.

The default build includes compression, which compiles two C libraries (libdeflate and MozJPEG) from source, so a C compiler is needed (nasm is not). For a reader-only build without the rendering and compression dependencies: `cargo build --no-default-features`; for rendering without compression: `cargo build --no-default-features --features render`.

### Code layout

| File | Purpose |
|---|---|
| `src/object.rs` | PDF object model (`Object`, `Dictionary`, `Stream`, `ObjRef`) |
| `src/parser.rs` | Lenient tokenizer and object parser |
| `src/xref.rs` | Cross-reference loading and damaged-file reconstruction |
| `src/filters.rs` | Stream decoding filters |
| `src/ccitt.rs` | CCITT Group 3/4 fax decoder |
| `src/document.rs` | `Document`: lazy object access, pages, metadata; unlocking protected files |
| `src/crypt/mod.rs` | Password-protected PDFs: the standard security handler (keys from passwords, decrypting and encrypting objects) |
| `src/crypt/aes.rs`, `src/crypt/hash.rs` | AES-128/256 with CBC; MD5, SHA-256, SHA-384, SHA-512 |
| `src/encoding.rs` | Text string decoding, base encodings, glyph names |
| `src/font.rs` | Font encodings, ToUnicode CMaps and widths (shared by text and rendering) |
| `src/std14.rs` | Glyph widths of the standard 14 fonts (generated from Adobe AFM files) |
| `src/edit/mod.rs` | Text editing: paragraphs, styled layout, applying edits |
| `src/edit/content.rs` | Content operations with byte positions; glyph removal |
| `src/edit/fonts.rs` | Fonts for new text: page fonts, system fonts, standard fonts; the handwriting fonts for typed signatures |
| `src/edit/subset.rs` | TrueType subsetting for embedded fonts |
| `src/compress/mod.rs` | Lossless compression: re-encoding streams, merging duplicates, Brotli marking |
| `src/compress/jpeg.rs` | Lossless JPEG optimization (MozJPEG), checked pixel for pixel |
| `src/edit/annotate.rs` | Comments: reading, adding, changing and deleting highlights and notes |
| `src/edit/redact.rs` | Redaction: removing text, image pixels, shapes and annotations in areas; finding phrases |
| `src/forms.rs` | Form fields: reading, filling, appearance streams |
| `src/edit/picture.rs` | Drawing a picture with transparency (a signature) into a page's content |
| `src/edit/fields.rs` | Making forms: adding, changing, moving and deleting fields; a blank document |
| `src/search.rs` | Finding phrases in page text: normalizing, whole words, approximate matching for OCR text |
| `src/ocr.rs` | Reading text in images (ocrs): finding the models, finding words in full-size tiles, levelling and turning images upright, reading the images of a page, memory |
| `src/scan/mod.rs` | Images and scanning: reading JPEG (with Exif orientation), telling formats apart, the pipeline, writing the PDF (scanned pages, and images kept as they are) |
| `src/scan/png.rs` | PNG reader (all color types and bit depths, interlacing) |
| `src/scan/formats.rs` | GIF, BMP, TIFF and WebP readers |
| `src/scan/detect.rs` | Finding the page in a photo: regions, Hough lines, scoring by edges, fitting the sides |
| `src/scan/signature.rs` | Lifting a signature off a photo or scan (paper made transparent, specks removed, one ink color); drawing one from strokes; writing a name in a font |
| `src/scan/clean.rs` | Flattening, evening out the light, straightening, contrast, black and white |
| `src/writer.rs` | Saving documents (pages rearranged, or merged from several documents) |
| `src/text.rs` | Content-stream interpreter for text extraction |
| `src/links.rs` | Link annotations, actions and destinations; the outline (bookmarks) |
| `src/layout.rs` | Reading order: splits lines into columns and blocks, moves line numbers aside |
| `src/render/mod.rs` | Content-stream interpreter for rendering (graphics state, paths, text, XObjects, annotations) |
| `src/render/color.rs` | Color spaces → sRGB |
| `src/render/function.rs` | PDF functions, including the PostScript calculator |
| `src/render/shading.rs` | Axial and radial shadings |
| `src/render/dark.rs` | Dark pages: OKLab lightness inversion keeping hues, photo detection |
| `src/render/image.rs` | Image decoding to on-screen size |
| `src/render/glyphs.rs` | Glyph outlines from embedded fonts; system font fallback |
| `src/render/type1.rs` | Type 1 font parser and charstring interpreter |
| `src/main.rs` | Command-line tool |
| `viewer/src/main.rs` | Desktop viewer UI |
| `viewer/src/worker.rs` | Viewer's background render threads (shared job queue) |
| `viewer/src/positions.rs` | Where reading left off in each file, kept on disk between sessions |
| `viewer/src/password.rs` | The password dialog, and the passwords given this session (used wherever a file is opened) |
| `viewer/src/selection.rs` | Text selection: hit-testing, word/line ranges, caret movement |
| `viewer/src/crop.rs` | Crop mode: choosing the area, crop panel |
| `viewer/src/rotate.rs` | Rotate mode: the rotate panel |
| `viewer/src/compress.rs` | The Compress dialog and the Brotli warning |
| `viewer/src/search.rs` | The search bar, the matches and stepping through them |
| `packaging/fetch-ocr-models.sh` | Downloads the text-recognition models (checked by SHA-256) |
| `viewer/src/dialog.rs` | File dialogs shown from a background thread, so the window keeps responding while one is open |
| `viewer/src/images.rs` | Images as a document: thumbnails, the look bar, corner handles and magnifier, previews, saving as one PDF |
| `viewer/src/present.rs` | Presenting: full-screen slides, transitions, the presenter view |
| `viewer/src/print/` | Printing: the Print dialog and preview (`mod.rs`), CUPS (`cups.rs`), Windows GDI (`windows.rs`) |
| `viewer/src/redact.rs` | Redact mode: marking areas, redact panel |
| `viewer/src/editor.rs` | Edit mode: in-place rich-text editor and formatting toolbar |
| `viewer/src/form.rs` | Editable form fields drawn over the page |
| `viewer/src/signature.rs` | Signing: the dialog (draw on a sheet, type in a handwriting font, or lift off a photo or scan), placing it on a page (moving, resizing) |
| `viewer/src/fields.rs` | Form mode: placing, selecting, moving and resizing fields; the form panel |
| `viewer/src/sidebar.rs` | Sidebar: page thumbnails, the sections, or the comments |
| `viewer/src/comments.rs` | Comments: the bar over selected text, the comment box |
| `viewer/src/script.rs` | Scripted input for testing the viewer (`script` feature) |
| `viewer/src/links.rs` | Link hit-testing, tooltips, which addresses may be opened |
| `viewer/src/titlebar.rs` | Custom title bar: window buttons, dragging, edge resizing |
| `packaging/linux/` | Launcher (`k-pdf.desktop`), icon, `install.sh`, `package.sh` |
| `packaging/windows/` | Program icon (`k-pdf.ico`, built into k-pdf.exe by `viewer/build.rs`), `package.sh` |
| `packaging/macos/` | App bundle: `Info.plist`, icon (`k-pdf.icns`), `bundle.sh` (universal app and `.dmg`) |
| `viewer/assets/icon-128.rgba` | Window icon (the SVG icon rendered at 128 px, raw RGBA) |
| `packaging/make-icons.py` | Makes the `.ico`, `.icns` and window icon from `packaging/linux/k-pdf.svg` |
| `bench/bench.py` | Benchmark against MuPDF, poppler, Ghostscript (and qpdf and desktop viewers when installed); draws the charts in `bench/results/` and rewrites the Benchmarks section of README.md |
| `bench/corpus.txt` | The public documents (TeX Live and R manuals) the benchmark uses |
| `viewer/src/theme.rs` | Dark and light palettes, styling, the system UI font |
| `viewer/src/icons.rs` | Vector toolbar icons and buttons |

### Testing
`cargo test --workspace` runs unit tests and integration tests that build PDFs byte by byte (`tests/reader.rs`, `tests/render.rs`).

The viewer can be tested by playing input from a script: build it with the `script` feature (kept out of normal builds) and pass `--script`. Each line is a step: `click x y`, `drag x0 y0 x1 y1`, `press x y` / `release x y` (a drag in steps, so a `shot` can be taken in the middle), `move x y`, `key ctrl+shift+h`, `text …`, `wait frames`, `pause ms` (frames can take under a millisecond, so background work such as scanning needs a pause), or `shot file.ppm` (saves the window once everything is rendered). A script may also start with images (`k-pdf a.jpg b.png --script …`): it then runs once they are read and shown. Script builds can't answer file dialogs, so they take the files to pick from `K_PDF_SCRIPT_PICK` (separated by `|`) and save images to `K_PDF_SCRIPT_SAVE`. The window closes at the end.

```sh
cargo build --release -p k-pdf-viewer --features script --target-dir target/script
./target/script/release/k-pdf paper.pdf --script steps.txt
```

The renderer was also checked against all 4,860 PDFs on the development machine. Every page 1 renders without error. In random samples of 150 pages, about 95% match MuPDF's output to within 0.5% of pixels.

Compression was checked on the 19 PDFs in Downloads and on 300 random PDFs from the development machine. Each file was compressed, and the copy was compared with the original:
- every page rendered by MuPDF matched pixel for pixel;
- the text, form fields and document info were identical.

Search was checked on the 3,962-page R reference manual (all pages in about a second; stepping through 339 matches in order) and on scanned PDFs made by k-pdf's scanner: OCR found the phrases on every scanned page, in the right places, including words inside a figure, and the scanned text could be selected. Of three "San Diego"s in a black-and-white scan, reading at 144 dpi got one, so pages are read at 180 dpi (all three), and matching OCR text allows a letter in six to differ.

Scanning was checked on synthetic phone photos (rendered pages warped in perspective onto wood, a dark desk and a light-gray table, with lighting gradients, a shadow, a color cast, noise and blur, one stored sideways with an Exif tag, and a flat scan slanted by 3°): the corners were found within about a pixel of the true ones, and the slanted scan was levelled. The PNG reader was compared with PIL on every PNG kind (palette, gray, 1- and 16-bit, gray and color with alpha, interlaced), and 400 random JPEG and PNG images from the development machine were scanned without an error or a hang. The GIF, BMP and TIFF readers were compared with PIL pixel for pixel (GIF plain and interlaced; BMP 1-, 8-, 24- and 32-bit; TIFF uncompressed, LZW, Deflate, PackBits, palette, gray, JPEG and fax G3/G4 both ways round; WebP lossy and lossless with alpha): identical, JPEG-in-TIFF within 1 level. 2,377 images on the development machine (631 GIF, 264 WebP, 87 BMP, 20 TIFF, the rest JPEG and PNG) were made into PDFs without a panic or a hang (6 failed: files named .png that aren't images), and so were 2,265 damaged copies (cut short, or with bytes changed) of the GIF, BMP and TIFF files. The viewer's image pages were checked with scripts: changing the look, placing corners (with the magnifier), turning, Use for all, undo, adding, reordering and removing pages, inserting images into a PDF, and saving (JPEGs copied in byte for byte).

Every file passed in both modes. The default mode made the 300 files 10.1% smaller (16.5% for the files in Downloads, where photos and repeated form parts are common); with Brotli, 22.7%.

#### Benchmarks

`python3 bench/bench.py` compares k-pdf with MuPDF (`mutool`), poppler (`pdftoppm`, `pdftotext`, `pdfunite`), Ghostscript, qpdf and the desktop viewers that are installed (MuPDF, Evince, Papers, Okular, zathura). It times rendering pages at 144 dpi, extracting text, merging, lossless compression, and the viewers' memory with a document open; it also checks that the renders match MuPDF's and poppler's pixels and that the extracted words match theirs. Then it redraws the charts in `bench/results/` (light and dark SVGs), saves the numbers in `bench/results/results.json` and rewrites the section between the `benchmarks:start` / `benchmarks:end` markers in README.md. Rerun it after changes that could affect speed or memory.

```sh
python3 bench/bench.py                    # everything (about 15 minutes; viewer windows open and close)
python3 bench/bench.py --only render,text # some tasks; the others keep their saved results
python3 bench/bench.py --no-viewers       # no windows
python3 bench/bench.py --report-only      # redraw charts and README from results.json
```

It needs Python 3 with matplotlib, numpy and Pillow, and GNU time (`/usr/bin/time`) for memory: the max RSS that `wait4` reports for a child also counts the memory of the parent that forked it, so it would show the script's own size for small tools. Each command runs once to warm the disk cache, then three times; the median time and the highest peak memory are kept. The documents are listed in `bench/corpus.txt`, with optional pages to render.

### Known limitations (planned for later phases)
- **Password-protected PDFs:** only the standard (password) security handler; files encrypted for certain people with certificates can't be opened. AES-256 passwords aren't SASLprep-normalized (plain passwords are unaffected; unusual Unicode ones may not match). The owner's permissions (no printing, no copying, …) aren't enforced, as in most open-source readers. Protection can't be added to or changed on a file, only kept or (with the CLI's `decrypt`) removed.
- **Cropping** only hides the rest of the page (as every PDF tool does); the hidden content is still in the file. Pages inserted from other PDFs can't be cropped or rotated until the file is saved and reopened.
- **Redaction** covers what the page itself draws. Some things are not changed:
  - document metadata (title, author, XMP), bookmarks, and alternate text in the structure tree;
  - the glyphs of redacted letters, which remain in embedded fonts;
  - shapes crossing an area's edge (they stay in the file, covered by the box);
  - patterns and shadings.

  Inline images and images that can't be decoded (JPX, JBIG2) are removed whole when an area touches them. A redacted copy of a JPEG image is saved losslessly, which can make the file larger. Pages inserted from other PDFs can't be redacted until the file is saved and reopened.
- **Text editing:** edited text has a fixed position on the page, so text that gets longer can run into what's below it (other content doesn't move). Text inside form XObjects, rotated or invisible text, and Type 3 fonts aren't editable. Fonts with CFF outlines (most `.otf` files) are embedded whole rather than subset, which makes files bigger. The on-screen editor shows text in the closest installed font; the saved page uses the real one. Small caps and ligatures become plain letters when edited.
- **Signing** puts a picture of a signature on the page, not a digital (certificate) signature: nothing proves who signed or that the file wasn't changed afterwards. Once placed, a signature is part of the page; to move it, undo and place it again (until the file is saved). The signature is taken from one box, so a signature crossing printed lines (a form's signature line) keeps the part of the line inside the box. It is kept only for the session. Typed signatures aren't shaped (no ligatures or joining alternates), so fonts that join letters through OpenType features show them unjoined; the fonts are the ones installed, so the choice differs between computers. Drawn strokes have one width (no pen pressure).
- **Forms:** JavaScript is not run, so calculated fields, formatting, validation and "gray hint" placeholder text don't work. Signature fields can't be signed, and rich text values are saved as plain text. Thumbnails show the values as last saved. Pages of inserted documents keep their fields as they were, without them being editable. Making forms: radio buttons, list boxes and signature fields can't be made yet (those from other programs can be moved, changed and deleted), a blank form has one page, and fields can't be placed on pages inserted from other files until the file is saved.
- **Merging** keeps the first document's metadata, bookmarks and form fields; those of inserted documents are not carried over (their pages, with links and annotations, are). Inserted password-protected PDFs are decrypted, and saved with the base document's protection (or none).
- **Rendering without vsync:** the viewer turns vsync off. With it, a window the compositor doesn't draw (covered, or on a screen that isn't shown) made every frame wait forever on Wayland, freezing the app. It repaints only when something changes, and animations are paced at about 60 frames a second, so this costs nothing when idle.
- **Printing** was tested on Linux with stand-in CUPS commands (no printer was attached): the dialog, the options passed to `lp` and the printed file were checked. The Windows code compiles and passes clippy but hasn't been run on a Windows printer yet. Printing on Linux and macOS needs CUPS (`lp`); without a printer set up, the dialog says none were found.
- **Compression** never loses anything, so its gains are modest on files that are already efficient. It does nothing for JPEG 2000, JBIG2 or CCITT images, which it copies as they are. It also doesn't subset fonts (unused glyphs are data too). Brotli files can't be opened by many readers, including Poppler-based ones (Evince, Okular, pdftotext), older Acrobat and Foxit, and many phone apps and printers. Brotli files are also no longer valid PDF/A. JPEG XL, which the PDF Association also plans to add, has no published specification for PDFs yet, so it isn't offered.
- **Saving** rewrites the whole file, so digital signatures no longer validate. (Links and bookmarks keep pointing to the same page wherever it moved.)
- **Images:** JPEG 2000 (JPX) and JBIG2 images are not decoded and are shown as gray boxes.
- **Text in images** is read for Latin script only (no Chinese, Arabic, Cyrillic…), and now and then a letter is misread, so a word in an image can be missed (approximate matching makes up for single letters in longer phrases). Pages scanned crooked (up to 15°), sideways or upside down are turned before reading, but within one image only the direction most of the text runs in is read well (a sideways label beside level text isn't), and an image with fewer than five words isn't turned upside up. Reading takes about a second a page, so in a long scanned document results from images come in over a while. Read text is kept in memory only, not saved into the file (it isn't an OCR text layer), and it can't be edited or redacted by text search: redact scanned pages by area. The models add about 12 MB to downloads, and rten about 6 MB to the program.
- **Images** are read in JPEG, PNG, GIF, BMP, TIFF and WebP; HEIC (iPhone) and AVIF photos need converting to JPEG first. Only the first frame of an animated GIF or WebP, and the first page of a TIFF with several, is read. TIFFs with separate color planes or old-style JPEG aren't read. Images are placed on A4 (or Letter or Legal when their proportions match), so small images are enlarged. Color profiles are ignored.
- **Handwriting** cleanup darkens what is faint, so on a page that is mostly printed it makes text bolder; it is a per-page choice. Ruled lines as dark as the writing are kept as ink. A word written almost invisibly (a single faint pass of pencil) can still come out patchy in black and white.
- **Scanning**: finding the page needs its edges to show against the background: a white page on a white table, or a page filling the whole photo, gives the whole photo, and the corners then need dragging. Curved pages (an open book) are flattened as planes, so lines near the spine stay curved. Scanned pages are images, so their text is found and selected through OCR (see above) rather than stored in the file. Black and white pages use Flate, not CCITT G4 or JBIG2, so they are a few times larger than a scanner's.
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

