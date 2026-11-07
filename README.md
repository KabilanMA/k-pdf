<p align="center">
  <img src="packaging/linux/k-pdf.svg" width="128" height="128" alt="k-pdf logo">
</p>

# k-pdf

A fast, lightweight PDF viewer and editor for Linux, Windows and macOS.

## What it does

- **Read** — smooth scrolling, page thumbnails, zoom, clickable links, and text you can select and copy.
- **Jump between sections** — the sidebar lists a document's headings and subheadings and highlights the one you're reading. Long documents open on it, short ones on the page thumbnails; one click switches.
- **Edit text** — click any paragraph and type. The text keeps its font, size and color, and you can change those as in a word processor.
- **Highlight and comment** — select text to highlight it in one of four colors or comment on it, or drop a note anywhere on a page. The sidebar lists all the comments. They're saved as standard PDF comments, so other PDF readers show them too.
- **Fill in forms** — text fields, check boxes, radio buttons and lists.
- **Rearrange pages** — drag thumbnails to reorder pages, or insert another PDF between any two pages.
- **Crop** — one page, several, or all of them to the same area.
- **Redact** — black out text, images or areas for good. The text under the box is removed from the file, not just covered.
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

Open a PDF with the **Open** button, by dropping a file onto the window, or with **Ctrl+O** (**Cmd+O** on a Mac). The toolbar has the editing tools: **Edit text**, **Crop**, **Redact** and **Comment**. The **⋯** menu has **Save as**, **Compress**, document info and the theme. Page navigation and zoom are at the bottom of the window.

| Shortcut | Action |
|---|---|
| Ctrl+O | Open a file |
| Ctrl+S | Save |
| Ctrl+Shift+S | Save as |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| Ctrl+E | Edit text on or off |
| Ctrl+Shift+D | Dark pages for reading on or off |
| Ctrl+Shift+B | Sidebar: pages, sections or comments |
| Ctrl+Shift+H | Highlight the selected text |
| Ctrl+C | Copy selected text |
| Ctrl + mouse wheel, Ctrl +/- | Zoom |
| Double-click a link | Go to that part of the document |
| Ctrl+click a web link | Open it in your browser |

On a Mac, use Cmd instead of Ctrl.

## Command line

k-pdf also comes with **k-pdf-cli** for scripts and batch jobs: merge, reorder, crop, redact, compress, fill forms, extract text and more.

```sh
k-pdf-cli merge   combined.pdf a.pdf b.pdf
k-pdf-cli redact  in.pdf out.pdf --text "Jane Doe"
k-pdf-cli compress in.pdf smaller.pdf
```

Run `k-pdf-cli --help` for everything it can do.

## Building from source

With [Rust](https://rustup.rs) and a C compiler installed:

```sh
cargo build --release --workspace
./target/release/k-pdf
```

On Linux, `packaging/linux/install.sh --link` adds it to your applications menu, linked to your build so every rebuild shows up.

How k-pdf works inside, how to build releases, the test results and the known limitations are in the [technical documentation](TECHNICAL.md).

## License

Licensed under either the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option.
