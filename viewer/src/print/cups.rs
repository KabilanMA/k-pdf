//! Printing through CUPS (Linux and macOS), with its command-line tools:
//! `lpstat` lists the printers, `lpoptions -l` tells what each one can do
//! (paper sizes, two-sided printing, color), and `lp` sends the job with
//! the chosen options. CUPS itself does the pages per sheet, scaling,
//! orientation and paper handling.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use super::{paper_label, paper_mm, Job, Orientation, Paper, Printer, Scaling, Sides};

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program).args(args).env("LC_ALL", "C").output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("printing needs CUPS, and its {program} command isn't installed")
        } else {
            format!("{program}: {e}")
        }
    })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // No printers at all is not an error.
        if err.contains("No destinations added") {
            return Ok(String::new());
        }
        return Err(if err.is_empty() { format!("{program} failed") } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn printers() -> Result<Vec<Printer>, String> {
    let names: Vec<String> = run("lpstat", &["-e"])?.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    // "system default destination: NAME"
    let default = run("lpstat", &["-d"])
        .ok()
        .and_then(|out| out.split_once(':').map(|(_, n)| n.trim().to_string()))
        .filter(|n| !n.is_empty());
    let descriptions = descriptions(&run("lpstat", &["-l", "-p"]).unwrap_or_default());
    Ok(names
        .into_iter()
        .map(|name| {
            let options = parse_options(&run("lpoptions", &["-p", &name, "-l"]).unwrap_or_default());
            let label = descriptions.get(&name).cloned().filter(|d| !d.is_empty()).unwrap_or_else(|| name.replace('_', " "));
            printer_from(name.clone(), label, default.as_deref() == Some(&name), &options)
        })
        .collect())
}

/// `lpstat -l -p`: each printer's "Description:".
fn descriptions(out: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut current: Option<String> = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("printer ") {
            current = rest.split_whitespace().next().map(String::from);
        } else if let (Some(name), Some(d)) = (&current, line.trim().strip_prefix("Description:")) {
            map.insert(name.clone(), d.trim().to_string());
        }
    }
    map
}

/// An option of `lpoptions -l`: its choices and the default one.
#[derive(Debug, Default, PartialEq)]
struct Choices {
    values: Vec<String>,
    default: Option<String>,
}

/// `lpoptions -p NAME -l`: lines like `PageSize/Media Size: *Letter A4`.
fn parse_options(out: &str) -> HashMap<String, Choices> {
    let mut map = HashMap::new();
    for line in out.lines() {
        let Some((key, choices)) = line.split_once(':') else { continue };
        let key = key.split('/').next().unwrap_or(key).trim().to_string();
        let mut c = Choices::default();
        for v in choices.split_whitespace() {
            match v.strip_prefix('*') {
                Some(d) => {
                    c.default = Some(d.to_string());
                    c.values.push(d.to_string());
                }
                None => c.values.push(v.to_string()),
            }
        }
        map.insert(key, c);
    }
    map
}

fn printer_from(name: String, label: String, is_default: bool, options: &HashMap<String, Choices>) -> Printer {
    let sizes = options.get("PageSize").or(options.get("media"));
    let papers = sizes
        .map(|c| {
            c.values
                .iter()
                .filter(|v| !v.starts_with("Custom"))
                .map(|v| Paper { id: v.clone(), label: paper_label(v), size_mm: paper_mm(v) })
                .collect()
        })
        .unwrap_or_default();
    let duplex = options.get("Duplex").is_some_and(|c| c.values.iter().any(|v| v.starts_with("Duplex")))
        || options.get("sides").is_some_and(|c| c.values.iter().any(|v| v.starts_with("two-sided")));
    // Color: a ColorModel / print-color-mode with something besides gray.
    let is_gray = |v: &str| {
        let v = v.to_ascii_lowercase();
        v.contains("gray") || v.contains("grey") || v.contains("mono") || v == "black" || v == "kgray"
    };
    let (mut color, mut gray_option) = (true, None);
    for key in ["ColorModel", "print-color-mode", "ColorMode", "OutputMode"] {
        if let Some(c) = options.get(key) {
            color = c.values.iter().any(|v| !is_gray(v));
            gray_option = c.values.iter().find(|v| is_gray(v)).map(|v| format!("{key}={v}"));
            break;
        }
    }
    Printer {
        name,
        label,
        is_default,
        papers,
        default_paper: sizes.and_then(|c| c.default.clone()),
        duplex,
        color,
        gray_option,
    }
}

/// The `lp` command line for a job.
pub fn lp_args(file: &Path, job: &Job) -> Vec<String> {
    let s = &job.settings;
    let mut args = vec!["-d".to_string(), job.printer.name.clone(), "-t".into(), job.title.clone()];
    let mut option = |o: String| {
        args.push("-o".into());
        args.push(o);
    };
    if s.copies > 1 {
        option(format!("collate={}", s.collate));
    }
    option(
        match s.sides {
            Sides::One => "sides=one-sided",
            Sides::LongEdge => "sides=two-sided-long-edge",
            Sides::ShortEdge => "sides=two-sided-short-edge",
        }
        .into(),
    );
    match s.orientation {
        Orientation::Auto => {}
        Orientation::Portrait => option("orientation-requested=3".into()),
        Orientation::Landscape => option("orientation-requested=4".into()),
    }
    if s.per_sheet > 1 {
        option(format!("number-up={}", s.per_sheet));
    } else {
        match s.scaling {
            Scaling::Fit => {
                option("print-scaling=fit".into());
                option("fit-to-page".into());
            }
            Scaling::Actual => option("print-scaling=none".into()),
            Scaling::Shrink => option("print-scaling=auto-fit".into()),
        }
    }
    if let Some(paper) = &s.paper {
        option(format!("media={paper}"));
    }
    if s.gray {
        option("print-color-mode=monochrome".into());
        if let Some(o) = &job.printer.gray_option {
            option(o.clone());
        }
    }
    args.push("-n".into());
    args.push(s.copies.max(1).to_string());
    args.push("--".into());
    args.push(file.to_string_lossy().into_owned());
    args
}

pub fn print(file: &Path, job: &Job) -> Result<String, String> {
    let args = lp_args(file, job);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run("lp", &refs)?;
    // "request id is NAME-42 (1 file(s))"
    let id = out.split_whitespace().skip_while(|w| *w != "is").nth(1).map(String::from);
    let pages = job.positions.len();
    let what = if pages == 1 { "1 page".to_string() } else { format!("{pages} pages") };
    Ok(match id {
        Some(id) => format!("Sent {what} to {} (job {id}).", job.printer.label),
        None => format!("Sent {what} to {}.", job.printer.label),
    })
}

#[cfg(test)]
mod tests {
    use super::super::Settings;
    use super::*;

    const LPOPTIONS: &str = "PageSize/Media Size: Letter *A4 Legal Executive na_index-4x6_4x6in Custom.WIDTHxHEIGHT\n\
        Duplex/2-Sided Printing: *None DuplexNoTumble DuplexTumble\n\
        ColorModel/Output Mode: Gray *RGB\n";

    #[test]
    fn printer_options() {
        let p = printer_from("HP_M479".into(), "HP Color LaserJet".into(), true, &parse_options(LPOPTIONS));
        assert_eq!(p.default_paper.as_deref(), Some("A4"));
        assert_eq!(p.papers.len(), 5);
        assert_eq!(p.papers[0].label, "Letter");
        assert_eq!(p.papers[4].size_mm, Some((101.6, 152.4)));
        assert!(p.duplex && p.color);
        assert_eq!(p.gray_option.as_deref(), Some("ColorModel=Gray"));

        let mono = parse_options("print-color-mode/Color Mode: *monochrome\nsides/2-Sided: *one-sided\n");
        let p = printer_from("Mono".into(), "Mono".into(), false, &mono);
        assert!(!p.color && !p.duplex);

        let d = descriptions("printer HP_M479 is idle.  enabled since …\n\tDescription: HP Color LaserJet\n\tLocation: Lab\n");
        assert_eq!(d["HP_M479"], "HP Color LaserJet");
    }

    #[test]
    fn lp_command() {
        let printer = printer_from("HP".into(), "HP".into(), true, &parse_options(LPOPTIONS));
        let settings = Settings {
            copies: 2,
            sides: Sides::LongEdge,
            per_sheet: 4,
            paper: Some("Letter".into()),
            gray: true,
            orientation: Orientation::Landscape,
            ..Settings::default()
        };
        let job = Job { printer, positions: vec![0, 1], settings, title: "a.pdf".into() };
        let args = lp_args(Path::new("/tmp/x.pdf"), &job).join(" ");
        assert_eq!(
            args,
            "-d HP -t a.pdf -o collate=true -o sides=two-sided-long-edge -o orientation-requested=4 -o number-up=4 \
             -o media=Letter -o print-color-mode=monochrome -o ColorModel=Gray -n 2 -- /tmp/x.pdf"
        );
    }
}
