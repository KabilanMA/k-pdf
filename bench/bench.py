#!/usr/bin/env python3
"""Benchmark k-pdf against other PDF tools and publish the results in README.md.

    python3 bench/bench.py                 # build, run everything, redraw charts, update README
    python3 bench/bench.py --only render,text --reps 3
    python3 bench/bench.py --no-viewers     # skip the desktop viewers (no windows open)
    python3 bench/bench.py --report-only    # redraw charts and README from bench/results/results.json

Tasks (each one runs every tool on every document in bench/corpus.txt):

    render    render pages to PNG at 144 dpi, one process per page
    fidelity  how many pixels of each render agree with MuPDF's and poppler's
    text      extract all the text of a document
    merge     combine all the documents into one
    compress  lossless size reduction (Ghostscript re-encodes images, so it is
              shown separately as lossy)
    viewer    memory of the desktop viewers with the document open, and k-pdf's
              time to show the first page

Every command is run once to warm the disk cache, then --reps times; the
median wall-clock time and the largest peak memory (max RSS, from wait4) are
kept. Tools that are not installed are left out. Results are kept in
bench/results/results.json, so a partial run (--only) updates its tasks and
keeps the others.

Needs Python 3.9+ with matplotlib, numpy and Pillow.
"""

import argparse
import datetime as dt
import json
import math
import os
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "bench"
RESULTS = BENCH / "results"
CORPUS = BENCH / "corpus.txt"
README = ROOT / "README.md"
KCLI = ROOT / "target" / "release" / "k-pdf-cli"
KVIEW = ROOT / "target" / "release" / "k-pdf"

START = "<!-- benchmarks:start -->"
END = "<!-- benchmarks:end -->"

DPI = 144
TASKS = ["render", "fidelity", "text", "merge", "compress", "viewer"]

# Each tool keeps one color in every chart (color follows the entity). The
# order and values are the validated default categorical palette; light and
# dark steps are selected separately for GitHub's light and dark pages.
TOOL_ORDER = ["k-pdf", "MuPDF", "poppler", "Ghostscript", "qpdf"]
VIEWER_ORDER = ["k-pdf", "MuPDF", "Evince", "Papers", "Okular", "zathura"]
PALETTE = {
    "light": ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#6250d6"],
    "dark": ["#3987e5", "#d95926", "#199e70", "#c98500", "#d55181", "#9085e9"],
}
THEME = {
    "light": dict(page="#ffffff", text="#1f2328", muted="#59636e", grid="#d8dee4"),
    "dark": dict(page="#0d1117", text="#f0f6fc", muted="#9198a1", grid="#30363d"),
}


# ---------------------------------------------------------------- running


def log(msg):
    print(msg, file=sys.stderr, flush=True)


# Peak memory comes from GNU time when it is installed. The rusage that wait4
# returns is not enough on Linux: a child's max RSS starts at its parent's RSS
# when it is forked, so every tool would report at least this script's own
# memory (which grows to over 100 MB while comparing renders). GNU time is a
# small process, so the RSS it reports belongs to the tool.
GNU_TIME = "/usr/bin/time" if Path("/usr/bin/time").exists() else None


def run_once(cmd, stdout=None, timeout=None):
    """Run a command; return (seconds, cpu seconds, peak RSS in bytes), None on failure or "timeout"."""
    import threading

    out = open(stdout, "wb") if stdout else subprocess.DEVNULL
    stats = tempfile.NamedTemporaryFile(prefix="k-pdf-bench-", suffix=".time", delete=False)
    stats.close()
    if GNU_TIME:
        cmd = [GNU_TIME, "-f", "%M %U %S", "-o", stats.name, *cmd]
    timed_out = []

    def kill():
        timed_out.append(True)
        p.kill()

    try:
        t0 = time.perf_counter()
        p = subprocess.Popen(cmd, stdout=out, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
        timer = threading.Timer(timeout, kill) if timeout else None
        if timer:
            timer.start()
        _, status, ru = os.wait4(p.pid, 0)  # blocking: polling would add its interval to short runs
        secs = time.perf_counter() - t0
        if timer:
            timer.cancel()
        p.returncode = os.waitstatus_to_exitcode(status)
        if timed_out:
            return "timeout"
        if p.returncode != 0:
            return None
        if GNU_TIME:
            mem, user, system = Path(stats.name).read_text().split()[-3:]
            return secs, float(user) + float(system), int(mem) * 1024
        return secs, ru.ru_utime + ru.ru_stime, ru.ru_maxrss * 1024
    finally:
        os.unlink(stats.name)
        if stdout:
            out.close()


def measure(cmd, reps, stdout=None, timeout=None):
    """Warm up once, then run `reps` times. Returns a result dict or an error string."""
    runs = []
    for i in range(reps + 1):
        r = run_once(cmd, stdout, timeout)
        if r is None:
            return "failed"
        if r == "timeout":
            return "timed out"
        if i == 0 and r[0] <= 20 and reps > 0:
            continue  # warm-up
        runs.append(r)
        if i == 0:
            break  # slow commands (over 20 s) aren't slowed by a cold cache: the warm-up run counts
    return {
        "time": statistics.median(r[0] for r in runs),
        "cpu": statistics.median(r[1] for r in runs),
        "mem": max(r[2] for r in runs),
    }


def have(exe):
    return shutil.which(exe) is not None


def version(cmd, pattern=r"(\d+(?:\.\d+)+)"):
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
        m = re.search(pattern, out.stdout + out.stderr)
        return m.group(1) if m else "?"
    except Exception:
        return "?"


def page_count(pdf):
    for cmd, pat in (
        (["pdfinfo", str(pdf)], r"Pages:\s+(\d+)"),
        ([str(KCLI), "info", str(pdf)], r"Pages:\s+(\d+)"),
    ):
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=60).stdout
            m = re.search(pat, out)
            if m:
                return int(m.group(1))
        except Exception:
            pass
    return None


def load_corpus(path):
    docs = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = [part.strip() for part in line.split("|")]
        label, file, about = parts[:3]
        render = [int(x) for x in parts[3].split(",")] if len(parts) > 3 and parts[3] else None
        file = Path(os.path.expandvars(os.path.expanduser(file)))
        if not file.is_file():
            log(f"  skipping {label}: {file} not found")
            continue
        pages = page_count(file)
        if not pages:
            log(f"  skipping {label}: could not count its pages")
            continue
        doc = dict(label=label, path=str(file), about=about, pages=pages, bytes=file.stat().st_size)
        if render:
            doc["render"] = [p for p in render if 1 <= p <= pages]
        docs.append(doc)
    return docs


def sample_pages(n, k):
    """k pages spread over the document: the first, the last and evenly in between."""
    if n <= k:
        return list(range(1, n + 1))
    return sorted({1 + round(i * (n - 1) / (k - 1)) for i in range(k)})


# ---------------------------------------------------------------- tools


def render_cmds(pdf, page, prefix):
    """Each tool writes its PNG to <prefix>-<tool>.png."""
    cmds = {"k-pdf": [str(KCLI), "render", pdf, str(page), f"{prefix}-k-pdf.png", "--scale", str(DPI / 72)]}
    if have("mutool"):
        cmds["MuPDF"] = ["mutool", "draw", "-q", "-r", str(DPI), "-o", f"{prefix}-MuPDF.png", pdf, str(page)]
    if have("pdftoppm"):
        cmds["poppler"] = ["pdftoppm", "-r", str(DPI), "-f", str(page), "-l", str(page), "-png", "-singlefile", pdf, f"{prefix}-poppler"]
    if have("gs"):
        cmds["Ghostscript"] = [
            "gs", "-q", "-dNOPAUSE", "-dBATCH", "-dSAFER", "-sDEVICE=png16m", f"-r{DPI}",
            f"-dFirstPage={page}", f"-dLastPage={page}", "-dTextAlphaBits=4", "-dGraphicsAlphaBits=4",
            f"-sOutputFile={prefix}-Ghostscript.png", pdf,
        ]
    return cmds


def text_cmds(pdf, prefix):
    """Each value is (command, file its stdout goes to or None); the text ends up in <prefix>-<tool>.txt."""
    cmds = {"k-pdf": ([str(KCLI), "text", pdf], f"{prefix}-k-pdf.txt")}
    if have("mutool"):
        cmds["MuPDF"] = (["mutool", "draw", "-q", "-F", "txt", "-o", f"{prefix}-MuPDF.txt", pdf], None)
    if have("pdftotext"):
        cmds["poppler"] = (["pdftotext", "-q", "-enc", "UTF-8", pdf, f"{prefix}-poppler.txt"], None)
    return cmds


def merge_cmds(pdfs, out):
    cmds = {"k-pdf": [str(KCLI), "merge", out, *pdfs]}
    if have("mutool"):
        cmds["MuPDF"] = ["mutool", "merge", "-o", out, *pdfs]
    if have("pdfunite"):
        cmds["poppler"] = ["pdfunite", *pdfs, out]
    if have("qpdf"):
        cmds["qpdf"] = ["qpdf", "--empty", "--pages", *pdfs, "--", out]
    return cmds


def compress_cmds(pdf, out):
    """Lossless rewrites with each tool's strongest standard settings, plus Ghostscript."""
    cmds = {"k-pdf": [str(KCLI), "compress", pdf, out]}
    if have("mutool"):
        # -ggg (merge duplicate objects) takes over 3 minutes on the R manual, so it is left out.
        cmds["MuPDF"] = ["mutool", "clean", "-gg", "-z", "-f", "-i", "-Z", "-e", "100", pdf, out]
    if have("qpdf"):
        cmds["qpdf"] = ["qpdf", "--object-streams=generate", "--compress-streams=y", "--recompress-flate",
                        "--compression-level=9", pdf, out]
    if have("gs"):
        cmds["Ghostscript"] = ["gs", "-q", "-dNOPAUSE", "-dBATCH", "-dSAFER", "-sDEVICE=pdfwrite", "-o", out, pdf]
    return cmds


def viewer_cmds(pdf):
    cmds = {"k-pdf": [str(KVIEW), pdf]}
    for name, exe in (("MuPDF", "mupdf"), ("Evince", "evince"), ("Papers", "papers"),
                      ("Okular", "okular"), ("zathura", "zathura")):
        if have(exe):
            cmds[name] = [exe, pdf]
    return cmds


# ---------------------------------------------------------------- tasks


def bench_render(docs, args, tmp):
    res, renders = {}, {}
    for d in docs:
        pages = d.get("render") or sample_pages(d["pages"], args.pages)
        log(f"  render  {d['label']}: pages {pages}")
        per_tool = {}
        for page in pages:
            for tool, cmd in render_cmds(d["path"], page, f"{tmp}/r-{page}").items():
                r = measure(cmd, args.reps, timeout=args.timeout)
                per_tool.setdefault(tool, []).append(r)
                if isinstance(r, dict):
                    renders.setdefault(d["label"], {}).setdefault(page, {})[tool] = f"{tmp}/r-{page}-{tool}.png"
        res[d["label"]] = {}
        for tool, rs in per_tool.items():
            ok = [r for r in rs if isinstance(r, dict)]
            if len(ok) < len(rs):
                res[d["label"]][tool] = next(r for r in rs if not isinstance(r, dict))
            else:
                res[d["label"]][tool] = {
                    "time": statistics.mean(r["time"] for r in ok),
                    "cpu": statistics.mean(r["cpu"] for r in ok),
                    "mem": max(r["mem"] for r in ok),
                }
        # Compare pixels now, before the next document overwrites the files.
        res[d["label"]]["_fidelity"] = fidelity(renders.get(d["label"], {}))
    fid = {label: res[label].pop("_fidelity") for label in res}
    return res, fid


def load_rgb(path):
    from PIL import Image
    import numpy as np

    im = Image.open(path)
    if im.mode in ("RGBA", "LA", "P"):
        bg = Image.new("RGBA", im.size, (255, 255, 255, 255))
        bg.alpha_composite(im.convert("RGBA"))
        im = bg
    return np.asarray(im.convert("RGB"), dtype=np.int16)


def fidelity(pages):
    """Share of pixels within 24/255 (per channel) of the reference renderer's pixel."""
    import numpy as np

    scores = {}
    for ref in ("MuPDF", "poppler"):
        for tool in TOOL_ORDER:
            if tool == ref:
                continue
            vals = []
            for files in pages.values():
                if tool not in files or ref not in files:
                    continue
                a, b = load_rgb(files[tool]), load_rgb(files[ref])
                h, w = min(a.shape[0], b.shape[0]), min(a.shape[1], b.shape[1])
                if abs(a.shape[0] - b.shape[0]) > 2 or abs(a.shape[1] - b.shape[1]) > 2:
                    vals.append(0.0)  # a different page size is a real disagreement
                    continue
                diff = np.abs(a[:h, :w] - b[:h, :w]).max(axis=2)
                vals.append(float((diff <= 24).mean()))
            if vals:
                scores.setdefault(tool, {})[ref] = statistics.mean(vals)
    return scores


def words(path):
    try:
        text = Path(path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return Counter()
    return Counter(re.findall(r"\w+", text.lower()))


def agreement(a, b):
    """F1 of the two word multisets: 1 means the same words the same number of times."""
    total = sum(a.values()) + sum(b.values())
    return 2 * sum((a & b).values()) / total if total else 1.0


def bench_text(docs, args, tmp):
    res, agree = {}, {}
    for d in docs:
        log(f"  text    {d['label']}")
        res[d["label"]], outs = {}, {}
        for tool, (cmd, stdout) in text_cmds(d["path"], f"{tmp}/t").items():
            r = measure(cmd, args.reps, stdout=stdout, timeout=args.timeout)
            res[d["label"]][tool] = r
            if isinstance(r, dict):
                outs[tool] = words(f"{tmp}/t-{tool}.txt")
        agree[d["label"]] = {
            tool: {ref: agreement(outs[tool], outs[ref]) for ref in ("MuPDF", "poppler") if ref in outs and ref != tool}
            for tool in outs
        }
    return res, agree


def bench_merge(docs, args, tmp):
    pdfs = [d["path"] for d in docs]
    expected = sum(d["pages"] for d in docs)
    log(f"  merge   {len(pdfs)} documents, {expected} pages")
    res = {}
    for tool, cmd in merge_cmds(pdfs, f"{tmp}/merged.pdf").items():
        r = measure(cmd, args.reps, timeout=args.timeout)
        if isinstance(r, dict):
            out = Path(f"{tmp}/merged.pdf")
            r["bytes"] = out.stat().st_size
            got = page_count(out)
            if got != expected:
                r = f"wrong page count ({got} instead of {expected})"
            out.unlink()
        res[tool] = r
    return {"inputs": len(pdfs), "pages": expected, "bytes_in": sum(d["bytes"] for d in docs), "tools": res}


def bench_compress(docs, args, tmp):
    res = {}
    for d in docs:
        log(f"  compress {d['label']}")
        res[d["label"]] = {}
        for tool, cmd in compress_cmds(d["path"], f"{tmp}/c.pdf").items():
            r = measure(cmd, args.reps, timeout=args.timeout)
            if isinstance(r, dict):
                out = Path(f"{tmp}/c.pdf")
                r["bytes"] = out.stat().st_size
                r["ratio"] = r["bytes"] / d["bytes"]
                got = page_count(out)
                if got != d["pages"]:
                    r = f"wrong page count ({got} instead of {d['pages']})"
                out.unlink()
            res[d["label"]][tool] = r
    return res


def process_tree(pid):
    children = {}
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                ppid = int(Path(f"/proc/{p}/stat").read_text().rsplit(")", 1)[1].split()[1])
                children.setdefault(ppid, []).append(int(p))
            except (OSError, IndexError, ValueError):
                pass
    out, todo = [], [pid]
    while todo:
        q = todo.pop()
        out.append(q)
        todo.extend(children.get(q, []))
    return out


def peak_rss(pids):
    """Sum of the peak resident memory (VmHWM) of a process and its helpers."""
    total = 0
    for pid in pids:
        try:
            for line in Path(f"/proc/{pid}/status").read_text().splitlines():
                if line.startswith("VmHWM:"):
                    total += int(line.split()[1]) * 1024
        except OSError:
            pass
    return total


def bench_viewer(docs, args, tmp):
    if not (os.environ.get("WAYLAND_DISPLAY") or os.environ.get("DISPLAY")):
        log("  viewer: no display, skipped")
        return None
    startup, memory = {}, {}
    for d in docs:
        log(f"  viewer  {d['label']}")
        # Time until k-pdf has drawn the window with the first page (it then exits).
        startup[d["label"]] = measure([str(KVIEW), d["path"], "--screenshot", f"{tmp}/s.ppm"], args.reps,
                                      timeout=args.timeout)
        memory[d["label"]] = {}
        for name, cmd in viewer_cmds(d["path"]).items():
            vals = []
            for _ in range(max(1, min(args.reps, 2))):
                p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                     stdin=subprocess.DEVNULL, start_new_session=True)
                time.sleep(args.settle)
                vals.append(peak_rss(process_tree(p.pid)) if p.poll() is None else None)
                try:
                    os.killpg(p.pid, signal.SIGTERM)
                    p.wait(timeout=10)
                except (ProcessLookupError, subprocess.TimeoutExpired):
                    os.killpg(p.pid, signal.SIGKILL)
                    p.wait()
            vals = [v for v in vals if v]
            memory[d["label"]][name] = {"mem": statistics.median(vals)} if vals else "exited early"
    return {"startup": startup, "memory": memory, "settle": args.settle}


# ---------------------------------------------------------------- environment


def environment():
    cpu = platform.processor() or "?"
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    osname = platform.system()
    try:
        osname = re.search(r'PRETTY_NAME="([^"]+)"', Path("/etc/os-release").read_text()).group(1)
    except (OSError, AttributeError):
        pass
    mem = None
    try:
        mem = int(re.search(r"MemTotal:\s+(\d+)", Path("/proc/meminfo").read_text()).group(1)) * 1024
    except (OSError, AttributeError):
        pass
    commit = subprocess.run(["git", "describe", "--always", "--dirty"], cwd=ROOT, capture_output=True,
                            text=True).stdout.strip()
    versions = {"k-pdf": commit or "?"}
    if have("mutool"):
        versions["MuPDF"] = version(["mutool", "-v"])
    if have("pdftoppm"):
        versions["poppler"] = version(["pdftoppm", "-v"])
    if have("gs"):
        versions["Ghostscript"] = version(["gs", "--version"])
    if have("qpdf"):
        versions["qpdf"] = version(["qpdf", "--version"])
    for name, exe in (("Evince", "evince"), ("Papers", "papers"), ("Okular", "okular"), ("zathura", "zathura")):
        if have(exe):
            versions[name] = version([exe, "--version"])
    return {
        "date": dt.date.today().isoformat(),
        "cpu": cpu,
        "cores": os.cpu_count(),
        "memory": mem,
        "os": osname,
        "kernel": platform.release(),
        "versions": versions,
    }


# ---------------------------------------------------------------- formatting


def fmt_time(s):
    if s is None:
        return "–"
    if s < 1:
        return f"{s * 1000:.0f} ms" if s >= 0.01 else f"{s * 1000:.1f} ms"
    return f"{s:.1f} s" if s < 100 else f"{s:.0f} s"


def fmt_mem(b):
    if b is None:
        return "–"
    mb = b / 1e6
    return f"{mb:.0f} MB" if mb >= 10 else f"{mb:.1f} MB"


def fmt_size(b):
    return fmt_mem(b) if b < 1e9 else f"{b / 1e9:.2f} GB"


def fmt_pct(x):
    return f"{x * 100:.1f}%" if x < 0.999 else f"{x * 100:.2f}%"


def geomean(xs):
    xs = [x for x in xs if x and x > 0]
    return math.exp(sum(map(math.log, xs)) / len(xs)) if xs else None


def value(r, key):
    return r.get(key) if isinstance(r, dict) else None


def tools_in(table, order):
    seen = {t for row in table.values() for t in row}
    return [t for t in order if t in seen]


def cell(r, key, fmt):
    return fmt(r[key]) if isinstance(r, dict) else (r or "–")


def md_table(header, rows):
    """Columns of numbers are right-aligned, others left-aligned."""
    numeric = re.compile(r"^\**[\d.,]+ ?(ms|s|MB|GB|%)?\**$|^[–-]$|^reference$")
    align = ["--:" if i and rows and all(numeric.match(r[i]) for r in rows) else "---" for i in range(len(header))]
    out = ["| " + " | ".join(header) + " |", "|" + "|".join(align) + "|"]
    out += ["| " + " | ".join(row) + " |" for row in rows]
    return "\n".join(out)


def per_doc_table(table, docs, key, fmt, order, unranked=()):
    """One row per document; the best value of the tools not in `unranked` is bold."""
    tools = tools_in(table, order)
    rows = []
    for d in docs:
        if d["label"] not in table:
            continue
        row = table[d["label"]]
        vals = {t: value(row.get(t), key) for t in tools}
        best = min((v for t, v in vals.items() if v is not None and t not in unranked), default=None)
        cells = []
        for t in tools:
            c = cell(row.get(t), key, fmt)
            cells.append(f"**{c}**" if vals[t] is not None and vals[t] == best and t not in unranked else c)
        rows.append([d["label"]] + cells)
    return md_table(["Document"] + tools, rows)


# ---------------------------------------------------------------- charts


def setup_matplotlib():
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    plt.rcParams.update({
        "font.family": "DejaVu Sans",
        "font.size": 10,
        "svg.fonttype": "path",
        "svg.hashsalt": "k-pdf",  # stable ids, so unchanged charts give no git diff
        "axes.spines.top": False,
        "axes.spines.right": False,
        "axes.spines.left": False,
    })
    return plt


def style_axes(ax, theme, log_scale):
    t = THEME[theme]
    ax.set_facecolor("none")
    ax.tick_params(colors=t["muted"], length=0, labelsize=9)
    for label in ax.get_yticklabels():
        label.set_color(t["text"])
    ax.spines["bottom"].set_color(t["grid"])
    ax.grid(axis="x", color=t["grid"], linewidth=0.8)
    ax.set_axisbelow(True)
    if log_scale:
        ax.set_xscale("log")


def tick_formatter(fmt):
    from matplotlib.ticker import FuncFormatter

    def tick(x, _):
        if x == 0:
            return "0"
        return re.sub(r"\.0(?=\D|$)", "", fmt(x))  # "3.0 s" -> "3 s"

    return FuncFormatter(tick)


def dot_chart(plt, path_stem, title, docs, panels, order, palette_of):
    """One row per document, one dot per tool, one panel per measure (never two scales on one axis)."""
    labels = [d["label"] for d in docs if any(d["label"] in p["data"] for p in panels)]
    if not labels:
        return None
    tools = []
    for p in panels:
        for t in tools_in({k: v for k, v in p["data"].items() if k in labels}, order):
            if t not in tools:
                tools.append(t)
    tools = [t for t in order if t in tools]
    for theme in ("light", "dark"):
        t = THEME[theme]
        height = 0.62 * len(labels) + 1.5
        fig, axes = plt.subplots(1, len(panels), figsize=(4.4 * len(panels) + 1.2, height),
                                 sharey=True, squeeze=False)
        fig.patch.set_alpha(0)
        for ax, p in zip(axes[0], panels):
            style_axes(ax, theme, p.get("log", True))
            n = len(tools)
            for i, tool in enumerate(tools):
                ys, xs = [], []
                for j, label in enumerate(labels):
                    v = value(p["data"].get(label, {}).get(tool), p["key"])
                    if v is not None:
                        ys.append(j + (i - (n - 1) / 2) * 0.15)
                        xs.append(p.get("scale", 1) * v)
                ax.plot(xs, ys, "o", ms=7.5, color=palette_of(theme, tool), markeredgecolor=t["page"],
                        markeredgewidth=1.4, label=tool, zorder=3, linestyle="none")
            ax.set_title(p["title"], loc="left", color=t["text"], fontsize=10, pad=8)
            ax.xaxis.set_major_formatter(tick_formatter(p["fmt"]))
            if p.get("log", True):
                from matplotlib.ticker import LogLocator

                lo, hi = ax.get_xlim()
                decades = math.log10(hi / lo)
                subs = (1.0, 2.0, 5.0) if decades < 1.6 else (1.0, 3.0) if decades < 3 else (1.0,)
                ax.xaxis.set_major_locator(LogLocator(base=10, subs=subs))
                ax.xaxis.set_minor_locator(LogLocator(base=10, subs=()))
            else:
                ax.set_xlim(left=0)
            ax.set_yticks(range(len(labels)))
            ax.set_yticklabels(labels)
            ax.set_ylim(len(labels) - 0.5, -0.5)
            for j in range(len(labels) - 1):
                ax.axhline(j + 0.5, color=t["grid"], linewidth=0.5, alpha=0.6)
        handles, names = axes[0][0].get_legend_handles_labels()
        for ax in axes[0][1:]:
            for h, nme in zip(*ax.get_legend_handles_labels()):
                if nme not in names:
                    handles.append(h)
                    names.append(nme)
        fig.suptitle(title, x=0.01, ha="left", va="top", color=t["text"], fontsize=11.5, fontweight="bold",
                     y=1 - 0.06 / height)
        leg = fig.legend(handles, names, loc="upper left", bbox_to_anchor=(0.005, 1 - 0.27 / height), ncol=len(names),
                         frameon=False, fontsize=9.5, handletextpad=0.2, columnspacing=1.2)
        for txt in leg.get_texts():
            txt.set_color(t["text"])
        fig.tight_layout(rect=(0, 0, 1, 1 - 0.42 / height))
        fig.savefig(RESULTS / f"{path_stem}-{theme}.svg", transparent=True, metadata={"Date": None})
        plt.close(fig)
    return path_stem


def bar_chart(plt, path_stem, title, panels, palette_of):
    """Horizontal bars, one per tool, for single measurements (such as merging everything)."""
    tools = [t for t in TOOL_ORDER if any(t in p["data"] for p in panels)]
    for theme in ("light", "dark"):
        t = THEME[theme]
        fig, axes = plt.subplots(1, len(panels), figsize=(3.6 * len(panels) + 0.9, 0.38 * len(tools) + 1.2),
                                 sharey=True, squeeze=False)
        fig.patch.set_alpha(0)
        for ax, p in zip(axes[0], panels):
            style_axes(ax, theme, False)
            vals = [p["scale"] * (p["data"].get(tool) or 0) for tool in tools]
            ax.barh(range(len(tools)), vals, height=0.5, color=[palette_of(theme, x) for x in tools], zorder=3)
            top = max(vals) if vals else 1
            for y, v in enumerate(vals):
                if v:
                    ax.text(v + top * 0.02, y, p["fmt"](v / p["scale"]), va="center", fontsize=9, color=t["text"])
            ax.set_xlim(0, top * 1.35)
            ax.set_title(p["title"], loc="left", color=t["text"], fontsize=10, pad=8)
            ax.xaxis.set_major_formatter(tick_formatter(lambda x, f=p["fmt"], s=p["scale"]: f(x / s)))
            ax.xaxis.set_major_locator(plt.MaxNLocator(3))
            ax.set_yticks(range(len(tools)))
            ax.set_yticklabels(tools)
            ax.set_ylim(len(tools) - 0.5, -0.5)
        fig.suptitle(title, x=0.01, ha="left", color=t["text"], fontsize=11.5, fontweight="bold")
        fig.tight_layout()
        fig.savefig(RESULTS / f"{path_stem}-{theme}.svg", transparent=True, metadata={"Date": None})
        plt.close(fig)
    return path_stem


def tool_color(theme, tool):
    return PALETTE[theme][TOOL_ORDER.index(tool)]


def viewer_color(theme, name):
    return PALETTE[theme][VIEWER_ORDER.index(name)]


def picture(stem, alt):
    return (f'<picture>\n  <source media="(prefers-color-scheme: dark)" srcset="bench/results/{stem}-dark.svg">\n'
            f'  <img alt="{alt}" src="bench/results/{stem}-light.svg">\n</picture>')


# ---------------------------------------------------------------- report


def summary_rows(R, docs):
    """Rows of (task, unit-formatter, {tool: value}, lower_is_better)."""
    rows = []

    def geo(table, key):
        out = {}
        for tool in TOOL_ORDER:
            vals = [value(table[d["label"]].get(tool), key) for d in docs if d["label"] in table]
            if vals and all(v is not None for v in vals):
                out[tool] = geomean(vals)
        return out

    if "render" in R:
        rows.append(("Render a page (time)", fmt_time, geo(R["render"], "time"), True))
        rows.append(("Render a page (peak memory)", fmt_mem, geo(R["render"], "mem"), True))
    if "fidelity" in R:
        avg = {}
        for tool in TOOL_ORDER:
            vals = [R["fidelity"][d["label"]].get(tool, {}).get("MuPDF") for d in docs if d["label"] in R["fidelity"]]
            vals = [v for v in vals if v is not None]
            if vals:
                avg[tool] = statistics.mean(vals)
        avg["MuPDF"] = "reference"
        rows.append(("Pixels matching MuPDF's render", fmt_pct, avg, False))
    if "text" in R:
        rows.append(("Extract all text (time)", fmt_time, geo(R["text"]["results"], "time"), True))
        rows.append(("Extract all text (peak memory)", fmt_mem, geo(R["text"]["results"], "mem"), True))
    if "merge" in R:
        m = R["merge"]["tools"]
        rows.append((f"Merge {R['merge']['inputs']} files, {R['merge']['pages']:,} pages (time)", fmt_time,
                     {t: value(r, "time") for t, r in m.items() if value(r, "time")}, True))
        rows.append(("Merge (peak memory)", fmt_mem, {t: value(r, "mem") for t, r in m.items() if value(r, "mem")},
                     True))
    if "compress" in R:
        lossless = {t: v for t, v in geo(R["compress"], "ratio").items() if t != "Ghostscript"}
        rows.append(("Lossless compression (output size)", fmt_pct, lossless, True))
        rows.append(("Lossless compression (time)", fmt_time,
                     {t: v for t, v in geo(R["compress"], "time").items() if t != "Ghostscript"}, True))
    return rows


def render_summary(rows):
    tools = [t for t in TOOL_ORDER if any(t in r[2] for r in rows)]
    lines = []
    for task, fmt, vals, lower in rows:
        numbers = [v for v in vals.values() if not isinstance(v, str)]
        best = (min if lower else max)(numbers) if numbers else None
        cells = []
        for t in tools:
            if t not in vals:
                cells.append("–")
                continue
            if isinstance(vals[t], str):
                cells.append(vals[t])
                continue
            c = fmt(vals[t])
            cells.append(f"**{c}**" if vals[t] == best else c)
        lines.append([task] + cells)
    return md_table(["Task"] + tools, lines)


def build_report(R):
    plt = setup_matplotlib()
    RESULTS.mkdir(parents=True, exist_ok=True)
    docs = R["docs"]
    env = R["env"]
    parts = []

    parts.append(
        "k-pdf compared with [MuPDF](https://mupdf.com), [poppler](https://poppler.freedesktop.org) "
        "(the engine of Evince, Okular and `pdftotext`), [Ghostscript](https://ghostscript.com) and the "
        "desktop viewers installed on the test machine, on "
        f"{len(docs)} public documents ({sum(d['pages'] for d in docs):,} pages). Lower is better everywhere "
        "except the pixel match; the best result in each row is in bold."
    )
    parts.append(render_summary(summary_rows(R, docs)))
    parts.append(
        "Times are the median of several runs after a warm-up, as the wall-clock time of the whole command "
        "(starting the program, opening the file and writing the result). Values across documents are "
        "geometric means. Memory is the peak resident memory of the process."
    )

    if "render" in R:
        stem = dot_chart(plt, "render", f"Rendering a page at {DPI} dpi", docs, [
            dict(title="Time per page", data=R["render"], key="time", fmt=fmt_time),
            dict(title="Peak memory", data=R["render"], key="mem", fmt=fmt_mem),
        ], TOOL_ORDER, tool_color)
        parts.append("### Rendering\n\n" + picture(stem, "Rendering time and memory per document")
                     + f"\n\nEach tool is started once per page and writes a PNG at {DPI} dpi; the "
                     f"pages are spread from the first to the last ({R['meta']['pages_per_doc']} per document), or named "
                     "in the corpus list (the photo book's photo is on page 3).")
    if "text" in R:
        stem = dot_chart(plt, "text", "Extracting all the text of a document", docs, [
            dict(title="Time", data=R["text"]["results"], key="time", fmt=fmt_time),
            dict(title="Peak memory", data=R["text"]["results"], key="mem", fmt=fmt_mem),
        ], TOOL_ORDER, tool_color)
        parts.append("### Text extraction\n\n" + picture(stem, "Text extraction time and memory per document")
                     + "\n\n`k-pdf-cli text`, `pdftotext` and `mutool draw -F txt`, all single-threaded.")
    if "compress" in R:
        stem = dot_chart(plt, "compress", "Making files smaller", docs, [
            dict(title="Output size (% of the original)", data=R["compress"], key="ratio", log=False,
                 scale=100, fmt=lambda x: f"{x:.0f}%"),
            dict(title="Time", data=R["compress"], key="time", fmt=fmt_time),
        ], TOOL_ORDER, tool_color)
        parts.append("### Compression\n\n" + picture(stem, "Compressed size and time per document")
                     + "\n\nk-pdf (`compress`, its default effort; `--best` is smaller still and slower), MuPDF "
                     "(`mutool clean -gg -z -f -i -Z -e 100`; `-ggg` takes over 3 minutes on the R manual) and qpdf "
                     "(when installed) keep every image and page exactly as it was. k-pdf recompresses images; MuPDF "
                     "copies them as they are, which is faster on files that are mostly images. Ghostscript "
                     "(`pdfwrite`) rebuilds the file and re-encodes images, so it is not lossless and is shown for "
                     "reference only.")
    if "merge" in R:
        m = R["merge"]["tools"]
        stem = bar_chart(plt, "merge", f"Merging {R['merge']['inputs']} files ({R['merge']['pages']:,} pages)", [
            dict(title="Time", data={t: value(r, "time") for t, r in m.items()}, scale=1, fmt=fmt_time),
            dict(title="Peak memory", data={t: value(r, "mem") for t, r in m.items()}, scale=1e-6,
                 fmt=fmt_mem),
            dict(title="Output size", data={t: value(r, "bytes") for t, r in m.items()}, scale=1e-6,
                 fmt=fmt_size),
        ], tool_color)
        parts.append("### Merging\n\n" + picture(stem, "Merge time, memory and output size")
                     + f"\n\nAll the documents merged into one, with `k-pdf-cli merge`, `mutool merge`, `pdfunite` "
                     f"and `qpdf --pages` (when installed). The inputs add up to {fmt_size(R['merge']['bytes_in'])}.")
    if R.get("viewer"):
        v = R["viewer"]
        order = [n for n in VIEWER_ORDER if any(n in row for row in v["memory"].values())]
        stem = dot_chart(plt, "viewer", "Desktop viewers: memory with the document open", docs, [
            dict(title=f"Peak memory after {v['settle']:g} s", data=v["memory"], key="mem", fmt=fmt_mem),
        ], order, viewer_color)
        startup = {d["label"]: value(v["startup"].get(d["label"]), "time") for d in docs}
        mem = {d["label"]: value(v["startup"].get(d["label"]), "mem") for d in docs}
        rows = [[d["label"], fmt_time(startup[d["label"]]), fmt_mem(mem[d["label"]])] for d in docs
                if d["label"] in v["startup"]]
        parts.append(
            "### Desktop app\n\n" + picture(stem, "Viewer memory per document")
            + "\n\nEach viewer opens the document and is left alone for "
            f"{v['settle']:g} seconds; the chart shows the peak memory of the app and its helper processes. "
            "The MuPDF viewer is a minimal window without a toolbar or sidebar. Time for k-pdf to start, open "
            "the document and show the first page:\n\n"
            + md_table(["Document", "Time to first page", "Peak memory"], rows)
        )

    # Every number, per document.
    details = []
    if "render" in R:
        details.append("**Rendering, time per page**\n\n" + per_doc_table(R["render"], docs, "time", fmt_time,
                                                                          TOOL_ORDER))
        details.append("**Rendering, peak memory**\n\n" + per_doc_table(R["render"], docs, "mem", fmt_mem,
                                                                        TOOL_ORDER))
    if "fidelity" in R:
        rows = []
        for d in docs:
            f = R["fidelity"].get(d["label"], {})
            for tool in TOOL_ORDER:
                if tool in f and tool != "MuPDF":  # MuPDF vs poppler is the poppler row
                    rows.append([d["label"], tool, fmt_pct(f[tool]["MuPDF"]) if "MuPDF" in f[tool] else "–",
                                 fmt_pct(f[tool]["poppler"]) if "poppler" in f[tool] else "–"])
        details.append("**Rendering, pixels that match** (each channel within 24 of 255)\n\n"
                       + md_table(["Document", "Renderer", "vs MuPDF", "vs poppler"], rows))
    if "text" in R:
        details.append("**Text extraction, time**\n\n" + per_doc_table(R["text"]["results"], docs, "time",
                                                                       fmt_time, TOOL_ORDER))
        details.append("**Text extraction, peak memory**\n\n" + per_doc_table(R["text"]["results"], docs, "mem",
                                                                              fmt_mem, TOOL_ORDER))
        rows = []
        for d in docs:
            a = R["text"]["agreement"].get(d["label"], {})
            for tool in TOOL_ORDER:
                if tool in a and tool != "MuPDF":  # MuPDF vs poppler is the poppler row
                    rows.append([d["label"], tool, fmt_pct(a[tool]["MuPDF"]) if "MuPDF" in a[tool] else "–",
                                 fmt_pct(a[tool]["poppler"]) if "poppler" in a[tool] else "–"])
        details.append("**Text extraction, words in common** (F1 score of the words found)\n\n"
                       + md_table(["Document", "Tool", "vs MuPDF", "vs poppler"], rows))
    if "compress" in R:
        sizes = {lab: {t: (dict(r, size=r["bytes"]) if isinstance(r, dict) else r) for t, r in row.items()}
                 for lab, row in R["compress"].items()}
        details.append("**Compression, output size** (Ghostscript is lossy, so it is not ranked)\n\n"
                       + per_doc_table(sizes, docs, "size", fmt_size, TOOL_ORDER, unranked=("Ghostscript",)))
        details.append("**Compression, time**\n\n" + per_doc_table(R["compress"], docs, "time", fmt_time,
                                                                   TOOL_ORDER, unranked=("Ghostscript",)))
    if "merge" in R:
        m = R["merge"]["tools"]
        rows = [[t, cell(m[t], "time", fmt_time), cell(m[t], "mem", fmt_mem), cell(m[t], "bytes", fmt_size)]
                for t in TOOL_ORDER if t in m]
        details.append("**Merging**\n\n" + md_table(["Tool", "Time", "Peak memory", "Output size"], rows))
    if R.get("viewer"):
        details.append("**Desktop viewers, peak memory**\n\n"
                       + per_doc_table(R["viewer"]["memory"], docs, "mem", fmt_mem, VIEWER_ORDER))
    corpus = md_table(["Document", "Pages", "Size", "What it is", "File"],
                      [[d["label"], f"{d['pages']:,}", fmt_size(d["bytes"]), d["about"], f"`{d['path']}`"]
                       for d in docs])
    details.append("**Documents** (listed in [bench/corpus.txt](bench/corpus.txt))\n\n" + corpus)
    parts.append("<details>\n<summary>All the numbers, per document</summary>\n\n" + "\n\n".join(details)
                 + "\n\n</details>")

    vers = ", ".join(f"{k} {v}" for k, v in env["versions"].items())
    mem = f", {env['memory'] / 2**30:.0f} GB RAM" if env.get("memory") else ""
    parts.append(
        f"Measured on {env['date']} on {env['cpu']} ({env['cores']} threads{mem}), {env['os']}, "
        f"Linux {env['kernel']}, with {vers}. Rerun with `python3 bench/bench.py` "
        "(see the top of [bench/bench.py](bench/bench.py) for the options); it rebuilds k-pdf, measures "
        "everything again, redraws the charts and rewrites this section."
    )
    body = "\n\n".join(parts)
    return f"{START}\n<!-- Generated by bench/bench.py. Edits between these markers are overwritten. -->\n\n{body}\n\n{END}"


def update_readme(section):
    text = README.read_text()
    if START in text and END in text:
        before, rest = text.split(START, 1)
        after = rest.split(END, 1)[1]
        text = before + section + after
    else:
        anchor = "## Building from source"
        block = f"## Benchmarks\n\n{section}\n\n"
        text = text.replace(anchor, block + anchor, 1) if anchor in text else text.rstrip() + "\n\n" + block
    README.write_text(text)


# ---------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--only", help="comma-separated tasks to run: " + ",".join(TASKS))
    ap.add_argument("--reps", type=int, default=3, help="timed runs per command after the warm-up (default 3)")
    ap.add_argument("--pages", type=int, default=3, help="pages rendered per document (default 3)")
    ap.add_argument("--timeout", type=float, default=300, help="seconds before a command counts as timed out")
    ap.add_argument("--settle", type=float, default=5, help="seconds each desktop viewer stays open (default 5)")
    ap.add_argument("--corpus", type=Path, default=CORPUS, help="list of documents (default bench/corpus.txt)")
    ap.add_argument("--no-build", action="store_true", help="use the existing release build")
    ap.add_argument("--no-viewers", action="store_true", help="skip the desktop viewer test (opens windows)")
    ap.add_argument("--no-readme", action="store_true", help="write results and charts but leave README.md alone")
    ap.add_argument("--report-only", action="store_true", help="only redraw charts and README from results.json")
    args = ap.parse_args()

    store = RESULTS / "results.json"
    R = json.loads(store.read_text()) if store.exists() else {}

    if not args.report_only:
        tasks = args.only.split(",") if args.only else list(TASKS)
        if args.no_viewers and "viewer" in tasks:
            tasks.remove("viewer")
        unknown = set(tasks) - set(TASKS)
        if unknown:
            sys.exit(f"Unknown task: {', '.join(sorted(unknown))}. The tasks are {', '.join(TASKS)}.")
        if not args.no_build:
            log("Building k-pdf (release)…")
            subprocess.run(["cargo", "build", "--release", "--workspace"], cwd=ROOT, check=True)
        if not KCLI.exists():
            sys.exit(f"{KCLI} is missing: build k-pdf first (cargo build --release --workspace).")

        log("Reading the corpus…")
        docs = load_corpus(args.corpus)
        if not docs:
            sys.exit("None of the documents in the corpus were found.")
        if R.get("docs") and [d["path"] for d in R["docs"]] != [d["path"] for d in docs]:
            log("  the corpus changed: dropping the old results")
            R = {}
        R["docs"], R["env"] = docs, environment()
        R.setdefault("meta", {})["pages_per_doc"] = args.pages

        with tempfile.TemporaryDirectory(prefix="k-pdf-bench-") as tmp:
            if "render" in tasks or "fidelity" in tasks:
                R["render"], R["fidelity"] = bench_render(docs, args, tmp)
            if "text" in tasks:
                res, agree = bench_text(docs, args, tmp)
                R["text"] = {"results": res, "agreement": agree}
            if "merge" in tasks and len(docs) > 1:
                R["merge"] = bench_merge(docs, args, tmp)
            if "compress" in tasks:
                R["compress"] = bench_compress(docs, args, tmp)
            if "viewer" in tasks:
                viewer = bench_viewer(docs, args, tmp)
                if viewer:
                    R["viewer"] = viewer

        RESULTS.mkdir(parents=True, exist_ok=True)
        store.write_text(json.dumps(R, indent=1) + "\n")
        log(f"Saved {store.relative_to(ROOT)}")

    if not R:
        sys.exit("No results yet: run without --report-only first.")
    section = build_report(R)
    log("Drew the charts in bench/results/")
    if not args.no_readme:
        update_readme(section)
        log("Updated README.md")


if __name__ == "__main__":
    main()
