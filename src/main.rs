use std::io::Write;
use std::process::ExitCode;

use k_pdf::{Document, ObjRef, Object, TextExtractor};

const USAGE: &str = "\
k-pdf-cli — the command-line tool of k-pdf, a small, low-memory PDF library

USAGE:
    k-pdf-cli info   <file.pdf>
    k-pdf-cli text   <file.pdf> [--page N]
    k-pdf-cli object <file.pdf> <num> [gen] [--decode]
    k-pdf-cli render <file.pdf> <page> <out.png> [--scale S] [--dark]
    k-pdf-cli reorder <in.pdf> <out.pdf> <pages>
    k-pdf-cli delete  <in.pdf> <out.pdf> <pages>
    k-pdf-cli merge   <out.pdf> <in1.pdf> <in2.pdf>...
    k-pdf-cli form    <file.pdf>
    k-pdf-cli outline <file.pdf>
    k-pdf-cli comments  <file.pdf>
    k-pdf-cli highlight <in.pdf> <out.pdf> <words> [--comment <text>]
    k-pdf-cli note      <in.pdf> <out.pdf> <page> <left> <top> <text>
    k-pdf-cli blocks  <file.pdf> <page>
    k-pdf-cli crop    <in.pdf> <out.pdf> <left> <top> <right> <bottom> [pages]
    k-pdf-cli rotate  <in.pdf> <out.pdf> <degrees> [pages]
    k-pdf-cli edit    <in.pdf> <out.pdf> <page> <block> <new text>
    k-pdf-cli redact  <in.pdf> <out.pdf> <page> <left> <top> <right> <bottom>
    k-pdf-cli redact  <in.pdf> <out.pdf> --text <words> [pages]
    k-pdf-cli compress <in.pdf> <out.pdf> [--brotli] [--no-jpeg]
    k-pdf-cli fill    <in.pdf> <out.pdf> <name=value>...
    k-pdf-cli blank   <out.pdf> [letter|a4|<width>x<height>]
    k-pdf-cli add-field <in.pdf> <out.pdf> <text|check|dropdown> <name> <page> <left> <top> <right> <bottom>
                      [--multiline] [--required] [--size PT] [--options a,b,c] [--tooltip <text>]
    k-pdf-cli search  <file.pdf> <words> [--case] [--words] [--no-images]
    k-pdf-cli ocr     <file.pdf> [--page N]
    k-pdf-cli scan    <out.pdf> <photo>... [--mode color|gray|bw|original] [--whole]
                      [--no-straighten] [--handwriting] [--corners x1,y1,x2,y2,x3,y3,x4,y4]
    k-pdf-cli images  <out.pdf> <image>...

COMMANDS:
    info     Show version, page count, metadata and page sizes
    text     Extract plain text (all pages, or one page with --page, 1-based)
    object   Print an object; with --decode, write a stream's decoded data to stdout
    render   Render a page (1-based) to a PNG; scale is pixels per point (default 2);
             --dark renders it as dark mode for reading does
    reorder  Save a copy with pages in a new order, e.g. \"3,1,2\" or \"5-10,1-4\"
             (pages left out are dropped)
    delete   Save a copy without some pages, e.g. \"2\" or \"1,4-6\" (their form fields go too)
    merge    Combine PDFs into one, in the order given
    form     List the form fields with their kinds and values
    outline  List the sections and subsections (bookmarks) with their pages
    comments List the comments: highlights, notes and other markup, with their text
    highlight Highlight every occurrence of some words, optionally with a comment
    note     Add a comment at a spot on a page (1-based; points from its top-left corner)
    fill     Fill in form fields: text as name=text, check boxes as name=on/off,
             radio buttons and choices as name=<option>
    blank    Make a PDF with one blank page (Letter, A4, or a size in points), to
             start a form from
    add-field Add a fillable field to a page (1-based; points from its top-left corner
             as shown): a text box (--multiline for several lines), a check box or a
             drop-down list (--options). --size sets the text size (default: fit
             to the field); --required marks it as needing an answer
    crop     Keep only the given area of pages (points from the top-left corner of the
             page as shown; all pages, or e.g. \"1,3-5\"); \"crop in out none\" uncrops
    rotate   Turn pages clockwise by 90, 180 or 270 degrees (negative: counterclockwise);
             all pages, or e.g. \"1,3-5\"
    blocks   List the editable text blocks of a page (1-based)
    edit     Replace a text block (numbered as by `blocks`) with new text, keeping
             its font, size and color
    redact   Permanently remove what is in an area of a page (points from its top-left
             corner as shown), or every occurrence of some words (all pages, or e.g.
             \"1,3-5\"): text, image pixels, shapes and annotations there are removed
             and a black box is drawn in their place
    compress Make a smaller copy without losing anything: duplicates merged, streams
             recompressed (libdeflate), JPEGs optimized losslessly (MozJPEG; --no-jpeg
             skips this). --brotli also allows Brotli compression (PDF 2.0 extension):
             smaller still, but many PDF readers can't open such files
    search   Find words: each place with its page and the text around it. Case and
             line breaks are ignored (--case matches case; --words whole words
             only). Text in images (scans, photos, figures) is read too, unless
             --no-images, when the text-recognition models are installed
    ocr      Print the text read in the images of each page (or one page)
    images   Make a PDF from images kept as they are, one page per image (JPEG, PNG,
             GIF, BMP, TIFF or WebP). JPEGs are copied in unchanged
    scan     Make a PDF from photos of pages (JPEG, PNG, GIF, BMP, TIFF or WebP), one
             page per photo, or clean up a scanned PDF (each of its pages, whole
             unless --corners): the page is found in each photo, flattened,
             straightened and cleaned up like a scan (--mode: color (default),
             gray, bw for black and white, or original). --handwriting darkens
             faint ink and pencil and keeps ruled lines light (and out of bw). --whole uses the whole photo; --corners gives the page's
             corners (top left, top right, bottom right, bottom left, in pixels)
             when there is one photo";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("info") if args.len() == 2 => info(&args[1]),
        Some("text") => text(&args[1..]),
        Some("object") => object(&args[1..]),
        Some("render") => render(&args[1..]),
        Some("reorder") if args.len() == 4 => reorder(&args[1], &args[2], &args[3]),
        Some("delete") if args.len() == 4 => delete(&args[1], &args[2], &args[3]),
        Some("merge") if args.len() >= 3 => merge(&args[1], &args[2..]),
        Some("form") if args.len() == 2 => form(&args[1]),
        Some("outline") if args.len() == 2 => outline(&args[1]),
        Some("comments") if args.len() == 2 => comments(&args[1]),
        Some("highlight") if args.len() == 4 || args.len() == 6 => highlight(&args[1..]),
        Some("note") if args.len() == 7 => note(&args[1..]),
        Some("blocks") if args.len() == 3 => blocks(&args[1], &args[2]),
        Some("crop") if args.len() >= 4 => crop(&args[1..]),
        Some("rotate") if args.len() == 4 || args.len() == 5 => rotate(&args[1..]),
        Some("redact") if args.len() >= 4 => redact(&args[1..]),
        Some("compress") if args.len() >= 3 => compress(&args[1..]),
        Some("edit") if args.len() == 6 => edit(&args[1], &args[2], &args[3], &args[4], &args[5]),
        Some("fill") if args.len() >= 4 => fill(&args[1], &args[2], &args[3..]),
        Some("blank") if args.len() == 2 || args.len() == 3 => blank(&args[1..]),
        Some("add-field") if args.len() >= 10 => add_field(&args[1..]),
        Some("scan") if args.len() >= 3 => scan(&args[1..]),
        Some("images") if args.len() >= 3 => images(&args[1..]),
        Some("search") if args.len() >= 3 => search(&args[1..]),
        Some("ocr") if args.len() >= 2 => ocr(&args[1..]),
        Some("-V" | "--version") => {
            println!("k-pdf-cli {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type CliResult = Result<(), Box<dyn std::error::Error>>;

fn reorder(input: &str, output: &str, spec: &str) -> CliResult {
    let doc = Document::open(input)?;
    let count = doc.page_count()?;
    let order = parse_page_list(spec, count)?;
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let file = std::fs::File::create(output)?;
    k_pdf::write_with_page_order(&doc, &order, file)?;
    println!("wrote {output} ({} pages)", order.len());
    Ok(())
}

fn delete(input: &str, output: &str, spec: &str) -> CliResult {
    let doc = Document::open(input)?;
    let count = doc.page_count()?;
    let gone = parse_page_list(spec, count)?;
    let keep: Vec<usize> = (0..count).filter(|p| !gone.contains(p)).collect();
    if keep.is_empty() {
        return Err("can't delete every page: a PDF needs at least one".into());
    }
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    k_pdf::write_with_page_order(&doc, &keep, std::fs::File::create(output)?)?;
    println!("wrote {output}: deleted {} pages, {} left", gone.len(), keep.len());
    Ok(())
}

fn crop(args: &[String]) -> CliResult {
    let (input, output) = (&args[0], &args[1]);
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let doc = Document::open(input)?;
    let count = doc.page_count()?;
    let (rect, rest) = if args[2] == "none" {
        (None, &args[3..])
    } else {
        let n: Vec<f32> = args.get(2..6).ok_or("expected <left> <top> <right> <bottom>")?.iter().map(|v| v.parse::<f32>()).collect::<Result<_, _>>()?;
        (Some([n[0], n[1], n[2], n[3]]), args.get(6..).unwrap_or(&[]))
    };
    let pages = match rest.first() {
        Some(spec) => parse_page_list(spec, count)?,
        None => (0..count).collect(),
    };
    let mut changes = k_pdf::Changes::default();
    for &p in &pages {
        k_pdf::edit::crop_page(&doc, p, rect, &mut changes)?;
    }
    let all: Vec<(usize, usize)> = (0..count).map(|p| (0, p)).collect();
    k_pdf::write_document_with_changes(&[&doc], &all, &changes, std::fs::File::create(output)?)?;
    println!("wrote {output} ({} pages cropped)", pages.len());
    Ok(())
}

fn rotate(args: &[String]) -> CliResult {
    let (input, output) = (&args[0], &args[1]);
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let degrees: i32 = args[2].parse().map_err(|_| format!("{:?} isn't a number of degrees", args[2]))?;
    let doc = Document::open(input)?;
    let count = doc.page_count()?;
    let pages = match args.get(3) {
        Some(spec) => parse_page_list(spec, count)?,
        None => (0..count).collect(),
    };
    let mut changes = k_pdf::Changes::default();
    for &p in &pages {
        k_pdf::edit::rotate_page(&doc, p, degrees, &mut changes)?;
    }
    let all: Vec<(usize, usize)> = (0..count).map(|p| (0, p)).collect();
    k_pdf::write_document_with_changes(&[&doc], &all, &changes, std::fs::File::create(output)?)?;
    println!("wrote {output}: {} pages turned {degrees}°", pages.len());
    Ok(())
}

fn redact(args: &[String]) -> CliResult {
    let (input, output) = (&args[0], &args[1]);
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let doc = Document::open(input)?;
    let count = doc.page_count()?;
    // Areas to redact on each page.
    let mut areas: Vec<(usize, Vec<[f32; 4]>)> = Vec::new();
    if args[2] == "--text" {
        let words = args.get(3).ok_or("expected the words to redact after --text")?;
        let pages = match args.get(4) {
            Some(spec) => parse_page_list(spec, count)?,
            None => (0..count).collect(),
        };
        for p in pages {
            let found = k_pdf::edit::find_text(&doc, p, words)?;
            if !found.is_empty() {
                areas.push((p, found));
            }
        }
        if areas.is_empty() {
            return Err(format!("{words:?} wasn't found").into());
        }
    } else {
        let page: usize = args[2].parse::<usize>().map_err(|_| "invalid page number")?;
        if page == 0 || page > count {
            return Err(format!("there is no page {page}").into());
        }
        let n: Vec<f32> = args.get(3..7).ok_or("expected <left> <top> <right> <bottom>")?.iter().map(|v| v.parse::<f32>()).collect::<Result<_, _>>()?;
        areas.push((page - 1, vec![[n[0], n[1], n[2], n[3]]]));
    }
    let mut changes = k_pdf::Changes::default();
    let mut total = k_pdf::edit::Redacted::default();
    for (p, a) in &areas {
        total += k_pdf::edit::redact_page(&doc, *p, a, &mut changes)?;
    }
    let all: Vec<(usize, usize)> = (0..count).map(|p| (0, p)).collect();
    k_pdf::write_document_with_changes(&[&doc], &all, &changes, std::fs::File::create(output)?)?;
    println!(
        "wrote {output}: {} areas on {} pages; removed {} characters, {} shapes, {} annotations; {} images blacked out, {} removed",
        areas.iter().map(|(_, a)| a.len()).sum::<usize>(),
        areas.len(),
        total.chars,
        total.shapes,
        total.annotations,
        total.images,
        total.images_removed
    );
    Ok(())
}

#[cfg(not(feature = "compress"))]
fn compress(_: &[String]) -> CliResult {
    Err("this build has no compression support (enable the `compress` feature)".into())
}

/// The text around `range` in `chars`, on one line.
fn context(chars: &[k_pdf::TextChar], range: &std::ops::Range<usize>) -> String {
    let piece = |r: std::ops::Range<usize>| k_pdf::chars_to_string(&chars[r]).split_whitespace().collect::<Vec<_>>().join(" ");
    let before = piece(range.start.saturating_sub(40)..range.start);
    let after = piece(range.end..(range.end + 40).min(chars.len()));
    let tail: String = before.chars().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
    let head: String = after.chars().take(30).collect();
    format!("…{tail} [{}] {head}…", piece(range.clone()))
}

fn search(args: &[String]) -> CliResult {
    let (path, words) = (&args[0], &args[1]);
    let flag = |f: &str| args[2..].iter().any(|a| a == f);
    let query = k_pdf::search::Query::new(words, flag("--case"), flag("--words")).ok_or("nothing to search for")?;
    let doc = Document::open(path)?;
    let pages = doc.pages()?;
    let mut extractor = TextExtractor::new(&doc);
    #[cfg(feature = "ocr")]
    let (mut ocr, mut renderer) = (None::<k_pdf::ocr::Ocr>, k_pdf::Renderer::new(&doc));
    let (mut total, mut in_images, mut found_pages, mut unread) = (0, 0, 0, 0);
    for (i, page) in pages.iter().enumerate() {
        let chars = extractor.extract_chars(page).unwrap_or_default();
        let mut hits: Vec<(String, bool)> = query.find(&chars).iter().map(|m| (context(&chars, &m.chars), false)).collect();
        #[cfg(feature = "ocr")]
        if !flag("--no-images") && k_pdf::ocr::page_has_images(&doc, page) {
            if ocr.is_none() {
                ocr = k_pdf::ocr::Ocr::find().ok();
            }
            match &ocr {
                Some(engine) => {
                    let read = k_pdf::ocr::read_page_images(engine, &doc, &mut renderer, page, &chars)?;
                    hits.extend(query.find_approximately(&read).iter().map(|m| (context(&read, &m.chars), true)));
                }
                None => unread += 1,
            }
        }
        if !hits.is_empty() {
            found_pages += 1;
        }
        for (text, image) in hits {
            total += 1;
            in_images += image as usize;
            println!("page {}: {text}{}", i + 1, if image { "  (in an image)" } else { "" });
        }
    }
    let s = |n: usize| if n == 1 { "" } else { "s" };
    println!("{total} match{} on {found_pages} page{}{}", if total == 1 { "" } else { "es" }, s(found_pages), if in_images > 0 { format!(" ({in_images} in images)") } else { String::new() });
    if unread > 0 {
        #[cfg(feature = "ocr")]
        eprintln!("note: images on {unread} page{} were not searched: {}", s(unread), k_pdf::ocr::not_installed());
    }
    Ok(())
}

#[cfg(not(feature = "ocr"))]
fn ocr(_: &[String]) -> CliResult {
    Err("this build can't read text in images (enable the `ocr` feature)".into())
}

#[cfg(feature = "ocr")]
fn ocr(args: &[String]) -> CliResult {
    let doc = Document::open(&args[0])?;
    let pages = doc.pages()?;
    let only = match args.get(1).map(String::as_str) {
        Some("--page") => Some(args.get(2).and_then(|p| p.parse::<usize>().ok()).filter(|&p| p >= 1 && p <= pages.len()).ok_or("--page needs a page number")? - 1),
        Some(other) => return Err(format!("unknown option {other:?}").into()),
        None => None,
    };
    let engine = k_pdf::ocr::Ocr::find()?;
    let mut renderer = k_pdf::Renderer::new(&doc);
    let mut extractor = TextExtractor::new(&doc);
    for (i, page) in pages.iter().enumerate().filter(|(i, _)| only.is_none_or(|p| p == *i)) {
        let chars = extractor.extract_chars(page).unwrap_or_default();
        let start = std::time::Instant::now();
        let read = k_pdf::ocr::read_page_images(&engine, &doc, &mut renderer, page, &chars)?;
        if read.is_empty() {
            continue;
        }
        println!("--- page {} ({:.1} s)", i + 1, start.elapsed().as_secs_f32());
        println!("{}", k_pdf::chars_to_string(&read));
    }
    Ok(())
}

#[cfg(not(feature = "compress"))]
fn scan(_: &[String]) -> CliResult {
    Err("this build has no scanning support (enable the `compress` feature)".into())
}

#[cfg(feature = "compress")]
fn scan(args: &[String]) -> CliResult {
    use k_pdf::scan::{self, Image, Mode, ScanOptions};
    let output = &args[0];
    let (mut photos, mut mode, mut whole, mut straighten, mut corners) = (Vec::new(), Mode::Color, false, true, None);
    let mut handwriting = false;
    let mut rest = args[1..].iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--mode" => {
                let name = rest.next().ok_or("--mode needs color, gray, bw or original")?;
                mode = Mode::from_name(name).ok_or_else(|| format!("unknown mode {name:?} (color, gray, bw or original)"))?;
            }
            "--whole" => whole = true,
            "--no-straighten" => straighten = false,
            "--handwriting" => handwriting = true,
            "--corners" => {
                let v: Vec<f32> = rest
                    .next()
                    .ok_or("--corners needs 8 numbers")?
                    .split(',')
                    .map(|n| n.trim().parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| "--corners needs 8 numbers separated by commas")?;
                if v.len() != 8 {
                    return Err("--corners needs 8 numbers separated by commas".into());
                }
                corners = Some([[v[0], v[1]], [v[2], v[3]], [v[4], v[5]], [v[6], v[7]]]);
            }
            _ => photos.push(a),
        }
    }
    if photos.is_empty() {
        return Err("no photos given".into());
    }
    if corners.is_some() && photos.len() > 1 {
        return Err("--corners can only be used with one photo".into());
    }
    let mut pdf = scan::PdfWriter::new(std::io::BufWriter::new(std::fs::File::create(output)?))?;
    let mut pages = 0;
    for path in &photos {
        // A PDF (a scan, or photos saved as a PDF): each page is scanned,
        // the whole page unless --corners are given.
        if path.to_lowercase().ends_with(".pdf") {
            let doc = Document::open(path.as_str())?;
            let mut renderer = k_pdf::Renderer::new(&doc);
            for (i, page) in doc.pages()?.iter().enumerate() {
                let image = scan::pdf_page_image(&doc, &mut renderer, page, scan::PDF_PAGE_PIXELS)?;
                let quad = corners.unwrap_or_else(|| scan::whole(&image));
                println!("{path}, page {}: {}×{}", i + 1, image.width, image.height);
                pdf.add_page(&scan::scan(&image, &ScanOptions { mode, straighten, handwriting, ..ScanOptions::new(quad) }))?;
                pages += 1;
            }
            continue;
        }
        pages += 1;
        let photo = Image::open(path).map_err(|e| format!("{path}: {e}"))?;
        let found = if whole || corners.is_some() { None } else { scan::find_page(&photo) };
        let quad = corners.or(found).unwrap_or_else(|| scan::whole(&photo));
        match (corners, found) {
            (Some(_), _) => println!("{path}: {}×{}, page at the given corners", photo.width, photo.height),
            (None, Some(q)) => {
                let q: Vec<String> = q.iter().map(|p| format!("{:.0},{:.0}", p[0], p[1])).collect();
                println!("{path}: {}×{}, page found at {}", photo.width, photo.height, q.join(" "));
            }
            (None, None) => println!("{path}: {}×{}, using the whole photo", photo.width, photo.height),
        }
        let page = scan::scan(&photo, &ScanOptions { mode, straighten, handwriting, ..ScanOptions::new(quad) });
        pdf.add_page(&page)?;
    }
    pdf.finish()?;
    println!("wrote {output}: {pages} page{}", if pages == 1 { "" } else { "s" });
    Ok(())
}

#[cfg(not(feature = "compress"))]
fn images(_: &[String]) -> CliResult {
    Err("this build can't make PDFs from images (enable the `compress` feature)".into())
}

#[cfg(feature = "compress")]
fn images(args: &[String]) -> CliResult {
    let output = &args[0];
    let mut pdf = k_pdf::scan::PdfWriter::new(std::io::BufWriter::new(std::fs::File::create(output)?))?;
    for path in &args[1..] {
        let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        pdf.add_image(&data, 0).map_err(|e| format!("{path}: {e}"))?;
    }
    pdf.finish()?;
    let n = args.len() - 1;
    println!("wrote {output}: {n} page{}", if n == 1 { "" } else { "s" });
    Ok(())
}

#[cfg(feature = "compress")]
fn compress(args: &[String]) -> CliResult {
    let (input, output) = (&args[0], &args[1]);
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let mut options = k_pdf::compress::CompressOptions::default();
    for flag in &args[2..] {
        match flag.as_str() {
            "--brotli" => options.brotli = true,
            "--no-jpeg" => options.jpeg = false,
            other => return Err(format!("unknown option {other}").into()),
        }
    }
    if options.brotli {
        eprintln!("warning: {}", BROTLI_WARNING);
    }
    let doc = Document::open(input)?;
    let before = std::fs::metadata(input)?.len();
    let mut file = std::io::BufWriter::new(std::fs::File::create(output)?);
    let report = k_pdf::compress::compress(&doc, &options, &mut file)?;
    file.flush()?;
    let pct = 100.0 * (1.0 - report.size as f64 / before.max(1) as f64);
    println!(
        "wrote {output}: {before} → {} bytes ({pct:.1}% smaller); {} streams recompressed ({} JPEGs, {} with Brotli), {} duplicates merged",
        report.size, report.streams, report.jpegs, report.brotli_streams, report.duplicates
    );
    if options.brotli && report.brotli_streams == 0 {
        println!("(Brotli didn't make any stream smaller, so the file stays compatible with every reader)");
    }
    Ok(())
}

#[cfg(feature = "compress")]
const BROTLI_WARNING: &str = "Brotli makes a PDF 2.0 file that only recent PDF readers can open \
(those supporting the PDF Association's 2026 Brotli extension). Older readers, many phone apps and \
printers will refuse it or show blank pages, and it is no longer valid PDF/A. Keep the original if \
you share the file.";

fn blocks(path: &str, page: &str) -> CliResult {
    let doc = Document::open(path)?;
    let page: usize = page.parse::<usize>().map_err(|_| "invalid page number")?.max(1) - 1;
    let text = k_pdf::edit::page_text(&doc, page)?;
    for (i, b) in text.blocks.iter().enumerate() {
        let s: String = text.block_text(b).into_iter().map(|(c, _)| c).collect();
        let style = b.glyphs.first().map(|&g| text.style_of(&text.glyphs[g]));
        let style = style.map_or(String::new(), |s| {
            format!("{} {}{}{}pt", s.family, if s.bold { "bold " } else { "" }, if s.italic { "italic " } else { "" }, s.size)
        });
        let flag = if b.editable { "" } else { " (not editable)" };
        let short: String = s.chars().take(70).collect();
        println!("{i:3}: [{style}, {} lines, {:?}]{flag} {short:?}", b.lines.len(), b.align);
    }
    Ok(())
}

fn edit(input: &str, output: &str, page: &str, block: &str, new_text: &str) -> CliResult {
    use k_pdf::edit::{BlockEdit, TextStyle};
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let doc = Document::open(input)?;
    let page: usize = page.parse::<usize>().map_err(|_| "invalid page number")?.max(1) - 1;
    let text = k_pdf::edit::page_text(&doc, page)?;
    let b = text.blocks.get(block.parse::<usize>().map_err(|_| "invalid block number")?).ok_or("no such block")?;
    let style: TextStyle = b.glyphs.first().map(|&g| text.style_of(&text.glyphs[g])).unwrap_or_default();
    let first = &b.lines[0];
    let edit = BlockEdit {
        page,
        remove: b.glyphs.clone(),
        runs: vec![(new_text.replace("\\n", "\n"), style)],
        left: b.lines.iter().map(|l| l.left).fold(f32::MAX, f32::min),
        width: b.rect[2] - b.rect[0],
        first_baseline: first.baseline,
        line_spacing: b.line_spacing,
        align: b.align,
        indent: b.indent,
        wrap: b.lines.len() > 1,
    };
    let mut changes = k_pdf::Changes::default();
    k_pdf::edit::edit_text(&doc, &edit, &mut changes)?;
    let pages: Vec<(usize, usize)> = (0..doc.page_count()?).map(|p| (0, p)).collect();
    k_pdf::write_document_with_changes(&[&doc], &pages, &changes, std::fs::File::create(output)?)?;
    println!("wrote {output}");
    Ok(())
}

fn comments(path: &str) -> CliResult {
    use k_pdf::edit::annotate::CommentKind;
    let doc = Document::open(path)?;
    let list = k_pdf::edit::annotate::comments(&doc)?;
    if list.is_empty() {
        println!("no comments");
    }
    for c in &list {
        let kind = match &c.kind {
            CommentKind::Highlight => "highlight".to_string(),
            CommentKind::Note => "note".to_string(),
            CommentKind::Other(k) => k.to_lowercase(),
        };
        let by = if c.author.is_empty() { String::new() } else { format!(" by {}", c.author) };
        println!("page {} {kind}{by}: {}", c.page + 1, c.text.replace('\n', " / "));
    }
    Ok(())
}

/// The name comments are signed with: the user's login name.
fn author() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default()
}

fn highlight(args: &[String]) -> CliResult {
    let (input, output, words) = (&args[0], &args[1], &args[2]);
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let comment = match args.get(3).map(String::as_str) {
        Some("--comment") => args[4].as_str(),
        Some(other) => return Err(format!("unexpected {other:?}").into()),
        None => "",
    };
    let doc = Document::open(input)?;
    let count = doc.page_count()?;
    let mut changes = k_pdf::Changes::default();
    let mut found = 0;
    for p in 0..count {
        for area in k_pdf::edit::find_text(&doc, p, words)? {
            let color = k_pdf::edit::annotate::HIGHLIGHT_YELLOW;
            k_pdf::edit::annotate::add_highlight(&doc, p, &[area], color, comment, &author(), &mut changes)?;
            found += 1;
        }
    }
    if found == 0 {
        return Err(format!("{words:?} wasn't found").into());
    }
    let all: Vec<(usize, usize)> = (0..count).map(|p| (0, p)).collect();
    k_pdf::write_document_with_changes(&[&doc], &all, &changes, std::fs::File::create(output)?)?;
    println!("wrote {output}: {found} highlights");
    Ok(())
}

fn note(args: &[String]) -> CliResult {
    let (input, output) = (&args[0], &args[1]);
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let doc = Document::open(input)?;
    let count = doc.page_count()?;
    let page: usize = args[2].parse().map_err(|_| "invalid page number")?;
    if page == 0 || page > count {
        return Err(format!("there is no page {page}").into());
    }
    let (x, y): (f32, f32) = (args[3].parse()?, args[4].parse()?);
    let mut changes = k_pdf::Changes::default();
    let color = k_pdf::edit::annotate::HIGHLIGHT_YELLOW;
    k_pdf::edit::annotate::add_note(&doc, page - 1, [x, y], color, &args[5], &author(), &mut changes)?;
    let all: Vec<(usize, usize)> = (0..count).map(|p| (0, p)).collect();
    k_pdf::write_document_with_changes(&[&doc], &all, &changes, std::fs::File::create(output)?)?;
    println!("wrote {output}");
    Ok(())
}

fn blank(args: &[String]) -> CliResult {
    let size = match args.get(1).map(|s| s.to_ascii_lowercase()) {
        None => (612.0, 792.0),
        Some(s) if s == "letter" => (612.0, 792.0),
        Some(s) if s == "a4" => (595.276, 841.89),
        Some(s) => {
            let (w, h) = s.split_once('x').ok_or("the size is letter, a4 or <width>x<height> in points")?;
            (w.parse::<f32>()?, h.parse::<f32>()?)
        }
    };
    if !(size.0 >= 3.0 && size.1 >= 3.0 && size.0 <= 14_400.0 && size.1 <= 14_400.0) {
        return Err("pages are 3 to 14,400 points on each side".into());
    }
    std::fs::write(&args[0], k_pdf::edit::fields::blank_document(size.0, size.1))?;
    println!("wrote {}", args[0]);
    Ok(())
}

fn add_field(args: &[String]) -> CliResult {
    use k_pdf::edit::fields::{FieldSpec, NewField};
    let (input, output) = (&args[0], &args[1]);
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let kind = match args[2].as_str() {
        "text" => NewField::Text,
        "check" | "checkbox" => NewField::CheckBox,
        "dropdown" => NewField::Dropdown,
        other => return Err(format!("unknown field kind \"{other}\" (text, check or dropdown)").into()),
    };
    let doc = Document::open(input)?;
    let count = doc.page_count()?;
    let page: usize = args[4].parse().map_err(|_| "invalid page number")?;
    if page == 0 || page > count {
        return Err(format!("there is no page {page}").into());
    }
    let mut rect = [0.0f32; 4];
    for (i, v) in args[5..9].iter().enumerate() {
        rect[i] = v.parse().map_err(|_| format!("invalid number \"{v}\""))?;
    }
    let mut spec = FieldSpec { name: args[3].clone(), ..FieldSpec::default() };
    let mut rest = args[9..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--multiline" => spec.multiline = true,
            "--required" => spec.required = true,
            "--size" => spec.font_size = rest.next().ok_or("--size needs a number")?.parse()?,
            "--options" => spec.options = rest.next().ok_or("--options needs a list")?.split(',').map(|o| o.trim().to_string()).collect(),
            "--tooltip" => spec.tooltip = rest.next().ok_or("--tooltip needs a text")?.clone(),
            other => return Err(format!("unknown option \"{other}\"").into()),
        }
    }
    let mut changes = k_pdf::Changes::default();
    k_pdf::edit::fields::add_field(&doc, page - 1, rect, kind, &spec, &mut changes)?;
    let all: Vec<(usize, usize)> = (0..count).map(|p| (0, p)).collect();
    k_pdf::write_document_with_changes(&[&doc], &all, &changes, std::fs::File::create(output)?)?;
    println!("wrote {output}");
    Ok(())
}

fn outline(path: &str) -> CliResult {
    let doc = Document::open(path)?;
    let pages = doc.pages()?;
    let items = k_pdf::outline(&doc, &pages);
    if items.is_empty() {
        println!("no outline");
    }
    for item in &items {
        let target = match &item.target {
            Some(k_pdf::LinkTarget::Page { page, .. }) => format!("page {}", page + 1),
            Some(k_pdf::LinkTarget::Uri(uri)) => uri.clone(),
            None => "-".into(),
        };
        println!("{}{}  ({target})", "  ".repeat(item.level), item.title);
    }
    Ok(())
}

fn form(path: &str) -> CliResult {
    use k_pdf::{FieldKind, FieldValue};
    let doc = Document::open(path)?;
    let fields = k_pdf::form_fields(&doc)?;
    if fields.is_empty() {
        println!("no form fields");
    }
    for f in &fields {
        let kind = match &f.kind {
            FieldKind::Text { multiline: true, .. } => "text (multi-line)".to_string(),
            FieldKind::Text { .. } => "text".to_string(),
            FieldKind::CheckBox => "check box".to_string(),
            FieldKind::Radio => {
                let states: Vec<_> = f.widgets.iter().filter_map(|w| w.on_state.clone()).collect();
                format!("radio [{}]", states.join(", "))
            }
            FieldKind::ComboBox { options, .. } | FieldKind::ListBox { options, .. } => {
                let o: Vec<_> = options.iter().map(|o| o.value.as_str()).collect();
                format!("choice [{}]", o.join(", "))
            }
            FieldKind::PushButton => "button".to_string(),
            FieldKind::Signature => "signature".to_string(),
        };
        let value = match &f.value {
            FieldValue::Text(t) => format!("{t:?}"),
            FieldValue::Button(Some(s)) => format!("on ({s})"),
            FieldValue::Button(None) => "off".to_string(),
            FieldValue::Choice(c) => format!("{c:?}"),
            FieldValue::None => String::new(),
        };
        let page = f.widgets.first().map_or(String::new(), |w| format!("p{} ", w.page + 1));
        let ro = if f.read_only { " (read-only)" } else { "" };
        println!("{page}{:?}: {kind}{ro} = {value}", f.name);
    }
    Ok(())
}

fn fill(input: &str, output: &str, assignments: &[String]) -> CliResult {
    use k_pdf::{FieldKind, FieldValue};
    if std::path::Path::new(input).canonicalize().ok() == std::path::Path::new(output).canonicalize().ok() {
        return Err("the output must be a different file than the input".into());
    }
    let doc = Document::open(input)?;
    let fields = k_pdf::form_fields(&doc)?;
    let mut values = Vec::new();
    for a in assignments {
        let (name, value) = a.split_once('=').ok_or_else(|| format!("expected name=value, got '{a}'"))?;
        let field = fields.iter().find(|f| f.name == name).ok_or_else(|| format!("no field named '{name}'"))?;
        let v = match &field.kind {
            FieldKind::Text { .. } => FieldValue::Text(value.replace("\\n", "\n")),
            FieldKind::CheckBox => match value {
                "on" | "yes" | "true" | "1" => FieldValue::Button(field.widgets.iter().find_map(|w| w.on_state.clone())),
                "off" | "no" | "false" | "0" => FieldValue::Button(None),
                state => FieldValue::Button(Some(state.to_string())),
            },
            FieldKind::Radio => FieldValue::Button((value != "off").then(|| value.to_string())),
            FieldKind::ComboBox { .. } | FieldKind::ListBox { .. } => {
                FieldValue::Choice(value.split('|').map(str::to_string).collect())
            }
            _ => return Err(format!("field '{name}' can't be filled in").into()),
        };
        values.push((field.id, v));
    }
    let changes = k_pdf::fill_form(&doc, &fields, &values)?;
    let pages: Vec<(usize, usize)> = (0..doc.page_count()?).map(|p| (0, p)).collect();
    k_pdf::write_document_with_changes(&[&doc], &pages, &changes, std::fs::File::create(output)?)?;
    println!("wrote {output} ({} fields filled)", values.len());
    Ok(())
}

fn merge(output: &str, inputs: &[String]) -> CliResult {
    let out_path = std::path::Path::new(output).canonicalize().ok();
    if inputs.iter().any(|i| std::path::Path::new(i).canonicalize().ok() == out_path && out_path.is_some()) {
        return Err("the output must be a different file than the inputs".into());
    }
    let docs: Vec<Document> = inputs.iter().map(Document::open).collect::<Result<_, _>>()?;
    let mut pages = Vec::new();
    for (d, doc) in docs.iter().enumerate() {
        pages.extend((0..doc.page_count()?).map(|p| (d, p)));
    }
    let refs: Vec<&Document> = docs.iter().collect();
    k_pdf::write_document(&refs, &pages, std::fs::File::create(output)?)?;
    println!("wrote {output} ({} pages)", pages.len());
    Ok(())
}

/// Parses "3,1,2" or "5-10,1-4" (1-based) into 0-based page indices.
fn parse_page_list(spec: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let page = |s: &str| -> Result<usize, String> {
            match s.trim().parse::<usize>() {
                Ok(n) if (1..=count).contains(&n) => Ok(n - 1),
                _ => Err(format!("invalid page '{s}' (the document has {count} pages)")),
            }
        };
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (page(a)?, page(b)?);
                if a <= b { out.extend(a..=b) } else { out.extend((b..=a).rev()) }
            }
            None => out.push(page(part)?),
        }
    }
    if out.is_empty() {
        return Err("no pages given".into());
    }
    Ok(out)
}

fn info(path: &str) -> CliResult {
    let doc = Document::open(path)?;
    let pages = doc.pages()?;
    println!("File:       {path}");
    println!("Version:    PDF {}", doc.version());
    println!("Pages:      {}", pages.len());
    println!("Objects:    {}", doc.object_count());
    println!("Encrypted:  {}", if doc.is_encrypted() { "yes" } else { "no" });
    if doc.was_repaired() {
        println!("Repaired:   yes (cross-reference table was damaged and rebuilt)");
    }
    match doc.metadata() {
        Ok(m) => {
            let fields = [
                ("Title", m.title),
                ("Author", m.author),
                ("Subject", m.subject),
                ("Keywords", m.keywords),
                ("Creator", m.creator),
                ("Producer", m.producer),
                ("Created", m.creation_date),
                ("Modified", m.mod_date),
            ];
            for (label, value) in fields {
                // Metadata is untrusted: keep control characters off the terminal.
                let clean = value.map(|v| v.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>());
                if let Some(v) = clean.filter(|v| !v.trim().is_empty()) {
                    println!("{:<11} {}", format!("{label}:"), v.trim());
                }
            }
        }
        Err(e) => println!("Metadata:   unavailable ({e})"),
    }
    if let Some(first) = pages.first() {
        let [x0, y0, x1, y1] = first.media_box(&doc);
        let (w, h) = (x1 - x0, y1 - y0);
        println!(
            "Page size:  {w:.0} x {h:.0} pt ({:.1} x {:.1} mm){}",
            w * 25.4 / 72.0,
            h * 25.4 / 72.0,
            if first.rotation() != 0 { format!(", rotated {}°", first.rotation()) } else { String::new() }
        );
    }
    Ok(())
}

fn text(args: &[String]) -> CliResult {
    let path = args.first().ok_or("missing file")?;
    let only_page: Option<usize> = match args.get(1).map(String::as_str) {
        Some("--page") => Some(args.get(2).ok_or("--page needs a number")?.parse()?),
        Some(other) => return Err(format!("unknown option '{other}'").into()),
        None => None,
    };
    let doc = Document::open(path)?;
    let pages = doc.pages()?;
    let mut extractor = TextExtractor::new(&doc);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let selected: Vec<(usize, &k_pdf::Page)> = match only_page {
        Some(n) if n >= 1 && n <= pages.len() => vec![(n, &pages[n - 1])],
        Some(n) => return Err(format!("page {n} out of range (document has {} pages)", pages.len()).into()),
        None => pages.iter().enumerate().map(|(i, p)| (i + 1, p)).collect(),
    };
    for (i, (_, page)) in selected.iter().enumerate() {
        if i > 0 {
            out.write_all(b"\x0C")?; // form feed between pages, like pdftotext
        }
        match extractor.extract_page(page) {
            Ok(t) => out.write_all(t.as_bytes())?,
            Err(e) => eprintln!("warning: page {}: {e}", selected[i].0),
        }
    }
    Ok(())
}

fn object(args: &[String]) -> CliResult {
    let path = args.first().ok_or("missing file")?;
    let num: u32 = args.get(1).ok_or("missing object number")?.parse()?;
    let rest = &args[2..];
    let decode = rest.iter().any(|a| a == "--decode");
    let generation: u16 = rest.iter().find(|a| *a != "--decode").map(|g| g.parse()).transpose()?.unwrap_or(0);

    let doc = Document::open(path)?;
    let obj = doc.get_object(ObjRef::new(num, generation))?;
    match (&obj, decode) {
        (Object::Stream(s), true) => {
            let decoded = doc.decode_stream_full(s)?;
            if let Some(f) = decoded.remaining_filter {
                eprintln!("note: data is still /{} encoded", String::from_utf8_lossy(&f));
            }
            std::io::stdout().write_all(&decoded.data)?;
        }
        (_, true) => return Err(format!("object {num} {generation} is a {}, not a stream", obj.type_name()).into()),
        _ => println!("{obj}"),
    }
    Ok(())
}

#[cfg(not(feature = "render"))]
fn render(_: &[String]) -> CliResult {
    Err("this build has no rendering support (enable the `render` feature)".into())
}

#[cfg(feature = "render")]
fn render(args: &[String]) -> CliResult {
    let [path, page, out, rest @ ..] = args else { return Err("usage: render <file.pdf> <page> <out.png>".into()) };
    let page: usize = page.parse()?;
    let (mut scale, mut dark) = (2.0, false);
    let mut options = rest.iter();
    while let Some(flag) = options.next() {
        match flag.as_str() {
            "--scale" => scale = options.next().ok_or("--scale needs a value")?.parse()?,
            "--dark" => dark = true,
            other => return Err(format!("unknown option {other}").into()),
        }
    }
    let doc = Document::open(path)?;
    let pages = doc.pages()?;
    let page = pages.get(page.wrapping_sub(1)).ok_or("page out of range")?;
    let start = std::time::Instant::now();
    let mut renderer = k_pdf::Renderer::new(&doc);
    renderer.set_dark_pages(dark);
    let img = renderer.render_page(page, scale)?;
    eprintln!("rendered {}x{} in {:.0} ms", img.width, img.height, start.elapsed().as_secs_f64() * 1000.0);
    let size = tiny_skia::IntSize::from_wh(img.width, img.height).ok_or("bad size")?;
    let pixmap = tiny_skia::Pixmap::from_vec(img.pixels, size).ok_or("bad pixmap")?;
    pixmap.save_png(out)?;
    Ok(())
}
