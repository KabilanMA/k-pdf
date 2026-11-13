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

## Benchmarks

<!-- benchmarks:start -->
<!-- Generated by bench/bench.py. Edits between these markers are overwritten. -->

k-pdf compared with [MuPDF](https://mupdf.com), [poppler](https://poppler.freedesktop.org) (the engine of Evince, Okular and `pdftotext`), [Ghostscript](https://ghostscript.com) and the desktop viewers installed on the test machine, on 5 public documents (4,544 pages). Lower is better everywhere except the pixel match; the best result in each row is in bold.

| Task | k-pdf | MuPDF | poppler | Ghostscript |
|---|--:|--:|--:|--:|
| Render a page (time) | **93 ms** | 94 ms | 186 ms | 131 ms |
| Render a page (peak memory) | 47 MB | 65 MB | **43 MB** | 55 MB |
| Pixels matching MuPDF's render | **98.3%** | reference | 97.2% | 96.8% |
| Extract all text (time) | **87 ms** | 655 ms | 542 ms | – |
| Extract all text (peak memory) | **25 MB** | 54 MB | 39 MB | – |
| Merge 5 files, 4,544 pages (time) | **549 ms** | 1.1 s | 3.7 s | – |
| Merge (peak memory) | **117 MB** | 190 MB | 143 MB | – |
| Lossless compression (output size) | **93.6%** | 99.0% | – | – |
| Lossless compression (time) | 7.3 s | **66 ms** | – | – |

Times are the median of several runs after a warm-up, as the wall-clock time of the whole command (starting the program, opening the file and writing the result). Values across documents are geometric means. Memory is the peak resident memory of the process.

### Rendering

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/render-dark.svg">
  <img alt="Rendering time and memory per document" src="bench/results/render-light.svg">
</picture>

Each tool is started once per page and writes a PNG at 144 dpi; the pages are spread from the first to the last (3 per document), or named in the corpus list (the photo book's photo is on page 3).

### Text extraction

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/text-dark.svg">
  <img alt="Text extraction time and memory per document" src="bench/results/text-light.svg">
</picture>

`k-pdf-cli text`, `pdftotext` and `mutool draw -F txt`, all single-threaded.

### Compression

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/compress-dark.svg">
  <img alt="Compressed size and time per document" src="bench/results/compress-light.svg">
</picture>

k-pdf (`compress`), MuPDF (`mutool clean -gg -z -f -i -Z -e 100`; `-ggg` takes over 3 minutes on the R manual) and qpdf (when installed) keep every image and page exactly as it was. Ghostscript (`pdfwrite`) rebuilds the file and re-encodes images, so it is not lossless and is shown for reference only.

### Merging

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/merge-dark.svg">
  <img alt="Merge time, memory and output size" src="bench/results/merge-light.svg">
</picture>

All the documents merged into one, with `k-pdf-cli merge`, `mutool merge`, `pdfunite` and `qpdf --pages` (when installed). The inputs add up to 73 MB.

### Desktop app

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/viewer-dark.svg">
  <img alt="Viewer memory per document" src="bench/results/viewer-light.svg">
</picture>

Each viewer opens the document and is left alone for 5 seconds; the chart shows the peak memory of the app and its helper processes. The MuPDF viewer is a minimal window without a toolbar or sidebar. Time for k-pdf to start, open the document and show the first page:

| Document | Time to first page | Peak memory |
|---|--:|--:|
| Manual | 127 ms | 94 MB |
| Maps | 175 ms | 104 MB |
| Symbols | 152 ms | 124 MB |
| Photo book | 466 ms | 392 MB |
| R manual | 282 ms | 163 MB |

<details>
<summary>All the numbers, per document</summary>

**Rendering, time per page**

| Document | k-pdf | MuPDF | poppler | Ghostscript |
|---|--:|--:|--:|--:|
| Manual | 45 ms | **34 ms** | 122 ms | 84 ms |
| Maps | 185 ms | **83 ms** | 260 ms | 337 ms |
| Symbols | **57 ms** | 103 ms | 150 ms | 92 ms |
| Photo book | 179 ms | **134 ms** | 218 ms | 180 ms |
| R manual | **81 ms** | 185 ms | 213 ms | 83 ms |

**Rendering, peak memory**

| Document | k-pdf | MuPDF | poppler | Ghostscript |
|---|--:|--:|--:|--:|
| Manual | **25 MB** | 33 MB | 28 MB | 41 MB |
| Maps | 37 MB | 43 MB | **30 MB** | 42 MB |
| Symbols | 35 MB | 59 MB | **32 MB** | 42 MB |
| Photo book | 173 MB | 153 MB | **139 MB** | 149 MB |
| R manual | 43 MB | 92 MB | **42 MB** | 45 MB |

**Rendering, pixels that match** (each channel within 24 of 255)

| Document | Renderer | vs MuPDF | vs poppler |
|---|---|--:|--:|
| Manual | k-pdf | 97.6% | 96.9% |
| Manual | poppler | 96.9% | – |
| Manual | Ghostscript | 97.2% | 96.2% |
| Maps | k-pdf | 98.0% | 94.3% |
| Maps | poppler | 94.5% | – |
| Maps | Ghostscript | 92.8% | 90.6% |
| Symbols | k-pdf | 98.4% | 96.8% |
| Symbols | poppler | 97.2% | – |
| Symbols | Ghostscript | 98.1% | 96.4% |
| Photo book | k-pdf | 99.4% | 99.4% |
| Photo book | poppler | 99.7% | – |
| Photo book | Ghostscript | 98.5% | 98.6% |
| R manual | k-pdf | 98.2% | 97.4% |
| R manual | poppler | 97.9% | – |
| R manual | Ghostscript | 97.6% | 96.8% |

**Text extraction, time**

| Document | k-pdf | MuPDF | poppler |
|---|--:|--:|--:|
| Manual | **24 ms** | 379 ms | 119 ms |
| Maps | **47 ms** | 283 ms | 307 ms |
| Symbols | **621 ms** | 3.6 s | 7.9 s |
| Photo book | **8.8 ms** | 30 ms | 27 ms |
| R manual | **803 ms** | 10.2 s | 6.0 s |

**Text extraction, peak memory**

| Document | k-pdf | MuPDF | poppler |
|---|--:|--:|--:|
| Manual | **8.0 MB** | 23 MB | 20 MB |
| Maps | 32 MB | 54 MB | **23 MB** |
| Symbols | **42 MB** | 112 MB | 84 MB |
| Photo book | 33 MB | 34 MB | **20 MB** |
| R manual | **27 MB** | 95 MB | 122 MB |

**Text extraction, words in common** (F1 score of the words found)

| Document | Tool | vs MuPDF | vs poppler |
|---|---|--:|--:|
| Manual | k-pdf | 99.98% | 99.7% |
| Manual | poppler | 99.7% | – |
| Maps | k-pdf | 96.7% | 97.8% |
| Maps | poppler | 96.4% | – |
| Symbols | k-pdf | 97.7% | 98.1% |
| Symbols | poppler | 98.3% | – |
| Photo book | k-pdf | 98.7% | 98.9% |
| Photo book | poppler | 99.3% | – |
| R manual | k-pdf | 99.99% | 99.5% |
| R manual | poppler | 99.5% | – |

**Compression, output size** (Ghostscript is lossy, so it is not ranked)

| Document | k-pdf | MuPDF | Ghostscript |
|---|--:|--:|--:|
| Manual | **1.1 MB** | 1.2 MB | 0.7 MB |
| Maps | **23 MB** | 24 MB | 17 MB |
| Symbols | **20 MB** | 22 MB | 22 MB |
| Photo book | **14 MB** | 14 MB | 14 MB |
| R manual | **10.0 MB** | 11 MB | 13 MB |

**Compression, time**

| Document | k-pdf | MuPDF | Ghostscript |
|---|--:|--:|--:|
| Manual | 2.6 s | **12 ms** | 281 ms |
| Maps | 30.2 s | **45 ms** | 1.8 s |
| Symbols | 8.2 s | **253 ms** | 12.0 s |
| Photo book | 5.2 s | **20 ms** | 452 ms |
| R manual | 6.4 s | **452 ms** | 19.5 s |

**Merging**

| Tool | Time | Peak memory | Output size |
|---|--:|--:|--:|
| k-pdf | 549 ms | 117 MB | 72 MB |
| MuPDF | 1.1 s | 190 MB | 74 MB |
| poppler | 3.7 s | 143 MB | 88 MB |

**Desktop viewers, peak memory**

| Document | k-pdf | MuPDF | Evince | Papers |
|---|--:|--:|--:|--:|
| Manual | 91 MB | **27 MB** | 122 MB | 200 MB |
| Maps | 102 MB | **33 MB** | 123 MB | 199 MB |
| Symbols | 122 MB | **52 MB** | 186 MB | 268 MB |
| Photo book | 390 MB | **25 MB** | 296 MB | 345 MB |
| R manual | 160 MB | **90 MB** | 231 MB | 297 MB |

**Documents** (listed in [bench/corpus.txt](bench/corpus.txt))

| Document | Pages | Size | What it is | File |
|---|--:|--:|---|---|
| Manual | 47 | 1.2 MB | 47-page manual: text, a few screenshots | `/usr/share/texlive/texmf-dist/doc/texlive/texlive-en/texlive-en.pdf` |
| Maps | 95 | 25 MB | 95 pages of vector maps and satellite images | `/usr/share/texlive/texmf-dist/doc/latex/mercatormap/mercatormap.pdf` |
| Symbols | 422 | 22 MB | 422 pages, hundreds of embedded fonts | `/usr/share/texlive/texmf-dist/doc/latex/comprehensive/symbols-a4.pdf` |
| Photo book | 18 | 14 MB | 18 pages with an 18-megapixel photo | `/usr/share/texlive/texmf-dist/doc/latex/willowtreebook/willowtreebook.pdf` |
| R manual | 3,962 | 11 MB | 3,962-page reference manual | `/usr/share/doc/R/manual/fullrefman.pdf` |

</details>

Measured on 2026-11-13 on Intel(R) Core(TM) Ultra 7 265 (20 threads, 31 GB RAM), Fedora Linux 43 (Workstation Edition), Linux 7.0.13-100.fc43.x86_64, with k-pdf v0.1.3-1-g362f63e, MuPDF 1.27.2, poppler 25.07.0, Ghostscript 10.05.1, Evince 48.1, Papers 49.6. Rerun with `python3 bench/bench.py` (see the top of [bench/bench.py](bench/bench.py) for the options); it rebuilds k-pdf, measures everything again, redraws the charts and rewrites this section.

<!-- benchmarks:end -->

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
