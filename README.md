<p align="center">
  <img src="packaging/linux/k-pdf.svg" width="128" height="128" alt="k-pdf logo">
</p>

# k-pdf

A fast, lightweight PDF viewer and editor for Linux, Windows and macOS.

## What it does

- **Read** — smooth scrolling, page thumbnails, zoom, clickable links, and text you can select and copy.
- **Open password-protected PDFs** — k-pdf asks for the password (the eye button shows what you typed) and opens the file. Everything works as usual: read, search, edit, sign, print. Saving keeps the same password, so the file stays protected. Files that are only protected against changes open without asking.
- **Find words** — Ctrl+F searches the whole document and highlights every match; Enter goes to the next. It also finds words in scanned pages, photos and pictures, which k-pdf reads with built-in text recognition (OCR), even when a page was scanned crooked, sideways or upside down. On scanned pages you can then select and copy the text too.
- **Pick up where you left off** — a file opens at the page and spot where you stopped reading it last time, even after closing k-pdf.
- **Jump between sections** — the sidebar lists a document's headings and subheadings and highlights the one you're reading. Long documents open on it, short ones on the page thumbnails; one click switches.
- **Edit text** — click any paragraph and type. The text keeps its font, size and color, and you can change those as in a word processor.
- **Highlight and comment** — select text to highlight it in one of four colors or comment on it, or drop a note anywhere on a page. The sidebar lists all the comments. They're saved as standard PDF comments, so other PDF readers show them too.
- **Fill in forms** — text fields, check boxes, radio buttons and lists. Fields you can fill in are shown in blue (a red border means it must be filled in); **Highlight fillable fields** in the ⋯ menu turns this off.
- **Create forms** — start from a blank page, or a PDF you have such as a paper form, and add text boxes, check boxes and drop-down lists that people can fill in. Drag to place a field, then move, resize, rename or delete it; other PDF readers can fill in the forms too.
- **Sign** — the pen button makes your signature three ways: **Draw** it on a white sheet with the mouse, a touchpad or a pen; **Type** your name and pick one of the handwriting fonts on your computer; or take it from a **Photo or scan** (an image or a PDF): drag a box around it, and the paper around it becomes see-through, leaving clean ink. Choose black or blue ink. Then drag it where it goes on the page and resize it by its corners; **Place** puts it there. Your signature is remembered while k-pdf is open, so you can sign several places quickly.
- **Rearrange pages** — drag thumbnails to reorder pages, insert another PDF between any two pages, or delete pages: one with the bin button on its thumbnail, or a list like "2, 5-7" with **Delete pages…** in the ⋯ menu.
- **Turn images into a PDF** — open JPEG, PNG, GIF, BMP, TIFF or WebP images just like a PDF: each image is a page, with thumbnails in the sidebar to add more images, reorder them or remove them. For each page, choose how it looks: **As is** keeps the image unchanged, while the scan looks turn a phone photo of a document into a clean scanned page. k-pdf finds the page in the photo, flattens it, straightens slanted text and removes shadows, so the paper is white and the writing crisp (**Color scan**, **Gray scan** or **Black & white**; **Cropped** only cuts the page out). Drag the corners if the page wasn't found exactly, then **Save PDF** to put all the pages in one PDF, and **Open PDF** to read the result. You can add PDFs too: their pages join the others, kept exactly as they are (text stays selectable) or cleaned up like a scan. Images can also be added to a PDF you have open (**Insert pages or images…** in the ⋯ menu, or the **+** between thumbnails): they open in the same editor first, so you can scan and crop each one, then **Insert** adds them as pages.
- **Clean up handwritten and scanned PDFs** — the sparkle button in the toolbar (or **Clean up scanned pages…** in the ⋯ menu) shows the document's pages as they are; pick a look for the ones that need it (**Apply to…** gives several pages the same look, like "1, 3-5" or all of them) and click **Apply**. The cleaned pages replace the old ones and you're back in the document with every other tool; Ctrl+Z undoes it. A blotchy, faint or grayish scan becomes clear and crisp. With **Handwriting** on, light pen and pencil strokes are darkened so every word is readable, the paper turns white, and a notebook's ruled lines stay light (and are left out entirely in black and white).
- **Crop** — one page, several, or all of them to the same area.
- **Rotate** — turn one page, a list of pages (like "1, 3-5"), or all of them.
- **Redact** — black out text, images or areas for good. Select some text and click **Redact** to mark it, or drag over any area. The text under the box is removed from the file, not just covered.
- **Present** — show the pages as full-screen slides with smooth fades between them, plus a presenter view with the next slide, your comments as speaker notes, and a timer. With two screens, the slides go on one and the presenter view on your main screen; you can switch them.
- **Print** — choose the printer, copies, which pages (all, the current one, a range like "1-3, 5", odd or even, in reverse), two-sided printing, several pages per sheet, scaling, paper size, black and white, and whether comments are printed, with a preview of each sheet. Unsaved changes are printed.
- **Compress** — make files smaller without losing any quality.
- **Undo and redo** everything, and save when you're ready.
- **Dark pages for reading** — the moon button turns pages dark: light text on a soft dark gray, with diagrams and colors kept recognizable (a light-blue box becomes a dark-blue one). Photos keep their real colors.
- **Light and dark themes** that follow your system.

It starts in a fraction of a second and uses little memory, even with large documents.

## Download

Get the latest version from the [Releases page](../../releases/latest):

| System | Download | How to install |
|---|---|---|
| Windows | `k-pdf-…-windows-x86_64.zip` | Unzip and run `k-pdf.exe`. If Windows SmartScreen warns you, choose **More info → Run anyway**. |
| macOS (Apple Silicon and Intel) | `k-pdf-…-macos-universal.dmg` | Open it and drag **k-pdf** into Applications. The first time, macOS blocks it: open **System Settings → Privacy & Security** and click **Open Anyway**. |
| Linux | `k-pdf-…-linux-x86_64.tar.gz` | Unpack it and run `./install.sh`. k-pdf then appears in your applications menu. Use `./install.sh --default` to make it your PDF app. |

The warnings on Windows and macOS appear because the app isn't signed with a paid developer certificate. They only show the first time.

## Using k-pdf

The start screen offers two things: **Open a PDF or images**, and **Create a form** (on a **Blank page**, or **From a PDF…**). You can also open a PDF with the **Open** button, by dropping a file onto the window, or with **Ctrl+O** (**Cmd+O** on a Mac). Opening or dropping images shows them as the pages of a new PDF (see above). The toolbar has the editing tools: **Edit text**, **Crop**, **Rotate**, **Redact**, **Form** and **Comment**. The **⋯** menu has **Save as**, **Insert pages or images**, **Print**, **Compress**, document info and the theme. Page navigation and zoom are at the bottom of the window, with buttons to fit the page's width or the whole page in the window.

| Shortcut | Action |
|---|---|
| Ctrl+O | Open a file |
| Ctrl+S | Save |
| Ctrl+Shift+S | Save as |
| Ctrl+F | Find words (also in scans and pictures) |
| Enter / Shift+Enter, F3 / Shift+F3 | Next / previous match |
| Ctrl+P | Print |
| F5 / Shift+F5 | Present from the start / from this page (→ ← to move, B black screen, P presenter view, Esc to end) |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| Ctrl+E | Edit text on or off |
| Ctrl+Shift+D | Dark pages for reading on or off |
| Ctrl+Shift+B | Sidebar: pages, sections or comments |
| Ctrl+Shift+H | Highlight the selected text |
| Ctrl+R / Ctrl+Shift+R | Rotate the current page right / left |
| Ctrl+C | Copy selected text |
| Ctrl + mouse wheel, Ctrl +/- | Zoom |
| Ctrl+0 / Ctrl+9 | Fit width / fit the whole page |
| Double-click a link | Go to that part of the document |
| Ctrl+click a web link | Open it in your browser |

On a Mac, use Cmd instead of Ctrl.

## Command line

k-pdf also comes with **k-pdf-cli** for scripts and batch jobs: merge, reorder, crop, redact, compress, fill forms, sign, make PDFs from images or scan photos, extract text and more.

```sh
k-pdf-cli merge   combined.pdf a.pdf b.pdf
k-pdf-cli redact  in.pdf out.pdf --text "Jane Doe"
k-pdf-cli compress in.pdf smaller.pdf
k-pdf-cli images  album.pdf photo1.jpg drawing.png
k-pdf-cli scan    receipts.pdf photo1.jpg photo2.jpg --mode bw
k-pdf-cli scan    clean-notes.pdf notes.pdf --handwriting
k-pdf-cli search  scanned.pdf "invoice number"
k-pdf-cli blank   form.pdf a4
k-pdf-cli add-field form.pdf signup.pdf text "Full name" 1 72 100 400 124 --required
k-pdf-cli sign    contract.pdf signed.pdf 2 72 640 150 --from signature-photo.jpg --area 400,500,1900,850
k-pdf-cli sign    contract.pdf signed.pdf 2 72 640 150 --text "Jane Doe" --ink blue
k-pdf-cli text    locked.pdf --password "the password"     # any command reads protected files
k-pdf-cli decrypt locked.pdf unlocked.pdf --password "the password"
```

Run `k-pdf-cli --help` for everything it can do.

## Building from source

With [Rust](https://rustup.rs) and a C compiler installed:

```sh
cargo build --release --workspace
./target/release/k-pdf
```

On Linux, `packaging/linux/install.sh --link` adds it to your applications menu, linked to your build so every rebuild shows up.

To find words in images, also run `packaging/fetch-ocr-models.sh` once: it downloads the text-recognition models (about 12 MB), which the release downloads already include.

How k-pdf works inside, how to build releases, the test results and the known limitations are in the [technical documentation](TECHNICAL.md).

## License

Licensed under either the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option.
