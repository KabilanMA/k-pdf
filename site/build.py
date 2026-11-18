#!/usr/bin/env python3
"""Builds the k-pdf website into a folder of static files for GitHub Pages.

    python3 site/build.py [--releases releases.json] [--out _site] [--repo owner/name]

The pages in site/pages/ are filled in with the latest release (version,
date and download links) and the news, then wrapped in site/layout.html.
Each published GitHub release becomes a news post, next to the hand-written
posts in site/news.json. The GitHub workflow .github/workflows/pages.yml
runs this after every release, so the site always links to the newest
version. Only the Python standard library is used.

Without --releases (or with an empty file) the site is built without
version details and the download buttons go to the Releases page.
"""

import argparse
import datetime as dt
import html
import json
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

# Release files, by how their names end (see packaging/*/).
PLATFORMS = [
    ("windows", "Windows", "-windows-x86_64.zip", "Windows 10 and 11, 64-bit"),
    ("macos", "macOS", "-macos-universal.dmg", "macOS 11 or later, Apple Silicon and Intel"),
    ("linux", "Linux", "-linux-x86_64.tar.gz", "64-bit, distributions from 2022 on"),
]


def esc(text):
    return html.escape(text, quote=True)


def repo_name(arg):
    if arg:
        return arg
    if os.environ.get("GITHUB_REPOSITORY"):
        return os.environ["GITHUB_REPOSITORY"]
    try:
        url = subprocess.run(["git", "remote", "get-url", "origin"], cwd=ROOT, capture_output=True, text=True).stdout
        m = re.search(r"github\.com[:/]([^/]+/[^/.\s]+)", url)
        if m:
            return m.group(1)
    except OSError:
        pass
    return "KabilanMA/k-pdf"


def load_releases(path):
    """Reads a JSON array, or one JSON object per line (`gh api --jq '.[]'`)."""
    if not path or not os.path.exists(path):
        return []
    text = open(path, encoding="utf-8").read().strip()
    if not text:
        return []
    if text.startswith("["):
        items = json.loads(text)
    else:
        items = [json.loads(line) for line in text.splitlines() if line.strip()]
    items = [r for r in items if not r.get("draft") and r.get("published_at")]
    items.sort(key=lambda r: r["published_at"], reverse=True)
    return items


def parse_date(stamp):
    return dt.datetime.strptime(stamp[:10], "%Y-%m-%d").date()


def nice_date(day):
    return f"{day.strftime('%B')} {day.day}, {day.year}"


def inline_md(text):
    """Markdown inside a line: links, bare URLs, bold, italics and code."""
    out = []
    pos = 0
    pattern = re.compile(r"`([^`]+)`|\[([^\]]+)\]\(([^)\s]+)\)|(https?://[^\s<)]+)|\*\*([^*]+)\*\*|(?<!\w)[*_]([^*_]+)[*_](?!\w)")
    for m in pattern.finditer(text):
        out.append(esc(text[pos:m.start()]))
        code, label, url, bare, bold, em = m.groups()
        if code is not None:
            out.append(f"<code>{esc(code)}</code>")
        elif label is not None:
            out.append(f'<a href="{esc(url)}">{inline_md(label)}</a>')
        elif bare is not None:
            shown = bare
            compare = re.search(r"/compare/([^/]+)$", bare)
            pull = re.search(r"/pull/(\d+)$", bare)
            if compare:
                shown = compare.group(1)
            elif pull:
                shown = "#" + pull.group(1)
            out.append(f'<a href="{esc(bare)}">{esc(shown)}</a>')
        elif bold is not None:
            out.append(f"<strong>{inline_md(bold)}</strong>")
        else:
            out.append(f"<em>{inline_md(em)}</em>")
        pos = m.end()
    out.append(esc(text[pos:]))
    return "".join(out)


def markdown(text):
    """Enough Markdown for GitHub release notes: headings, lists, paragraphs."""
    lines = text.replace("\r\n", "\n").split("\n")
    out, para, in_list = [], [], False

    def flush():
        nonlocal para
        if para:
            out.append("<p>" + inline_md(" ".join(para)) + "</p>")
            para = []

    for line in lines:
        stripped = line.strip()
        item = re.match(r"^[*+-]\s+(.*)", stripped)
        heading = re.match(r"^(#{1,6})\s+(.*)", stripped)
        if item:
            flush()
            if not in_list:
                out.append("<ul>")
                in_list = True
            out.append("<li>" + inline_md(item.group(1)) + "</li>")
            continue
        if in_list and (not stripped or heading):
            out.append("</ul>")
            in_list = False
        if heading:
            flush()
            level = min(len(heading.group(1)) + 2, 6)
            out.append(f"<h{level}>{inline_md(heading.group(2))}</h{level}>")
        elif not stripped:
            flush()
        else:
            para.append(stripped)
    flush()
    if in_list:
        out.append("</ul>")
    return "\n".join(out)


def commit_subjects(prev_tag, tag):
    """The commit messages between two tags, when the git history is there."""
    if not prev_tag:
        return []
    try:
        result = subprocess.run(
            ["git", "log", "--no-merges", "--format=%s", f"{prev_tag}..{tag}"],
            cwd=ROOT, capture_output=True, text=True, timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired):
        return []
    if result.returncode != 0:
        return []
    return [s.strip() for s in result.stdout.splitlines() if s.strip()][:30]


def downloads_of(release):
    found = {}
    for asset in release.get("assets", []):
        for key, _, suffix, _ in PLATFORMS:
            if asset["name"].endswith(suffix):
                found[key] = (asset["browser_download_url"], asset["name"], asset.get("size", 0))
    return found


def size_text(n):
    return f"{n / 1_000_000:.0f} MB" if n >= 1_000_000 else f"{max(n, 1) / 1000:.0f} KB"


def release_news(releases):
    """One news post per release, newest first."""
    posts = []
    for i, r in enumerate(releases):
        tag = r["tag_name"]
        version = tag.lstrip("v")
        prev = releases[i + 1]["tag_name"] if i + 1 < len(releases) else None
        notes = (r.get("body") or "").strip()
        # GitHub's generated notes are often only a "Full Changelog" link.
        only_link = not notes or re.fullmatch(r"\*\*Full Changelog\*\*:\s*\S+", notes)
        body = []
        if notes and not only_link:
            body.append(markdown(notes))
        else:
            subjects = commit_subjects(prev, tag)
            if subjects:
                body.append("<p>What changed:</p><ul>" + "".join(f"<li>{inline_md(s[:1].upper() + s[1:])}</li>" for s in subjects) + "</ul>")
            if notes:
                body.append(markdown(notes))
        title = r.get("name") or tag
        if title in (tag, version):
            title = f"k-pdf {version} is out"
        posts.append({
            "id": tag,
            "date": parse_date(r["published_at"]),
            "title": title,
            "html": "\n".join(body),
            "link": r["html_url"],
            "release": True,
            "version": version,
            "prerelease": r.get("prerelease", False),
        })
    return posts


def manual_news():
    path = os.path.join(HERE, "news.json")
    if not os.path.exists(path):
        return []
    posts = []
    for n in json.load(open(path, encoding="utf-8")):
        posts.append({
            "id": n.get("id") or re.sub(r"[^a-z0-9]+", "-", n["title"].lower()).strip("-"),
            "date": dt.date.fromisoformat(n["date"]),
            "title": n["title"],
            "html": markdown(n.get("body", "")),
            "link": n.get("link"),
            "release": False,
        })
    return posts


def news_post_html(post):
    tag = '<span class="tag">Release</span>' if post["release"] else '<span class="tag tag-plain">News</span>'
    if post.get("prerelease"):
        tag += ' <span class="tag tag-plain">Pre-release</span>'
    link = ""
    if post["release"]:
        link = f'<p class="post-actions"><a class="button small" href="{esc(post["link"])}">Download {esc(post["version"])}</a> <a class="text-link" href="{esc(post["link"])}">Release page on GitHub</a></p>'
    elif post.get("link"):
        link = f'<p class="post-actions"><a class="text-link" href="{esc(post["link"])}">Read more</a></p>'
    return f"""<article class="post" id="{esc(post['id'])}">
  <header><div class="post-meta">{tag}<time datetime="{post['date'].isoformat()}">{nice_date(post['date'])}</time></div>
  <h2><a href="#{esc(post['id'])}">{esc(post['title'])}</a></h2></header>
  <div class="post-body">{post['html']}</div>
  {link}
</article>"""


def atom_feed(posts, site_url):
    updated = posts[0]["date"].isoformat() if posts else dt.date.today().isoformat()
    entries = []
    for p in posts[:30]:
        url = f"{site_url}news.html#{p['id']}"
        entries.append(f"""  <entry>
    <title>{esc(p['title'])}</title>
    <link href="{esc(p.get('link') or url)}"/>
    <id>{esc(url)}</id>
    <updated>{p['date'].isoformat()}T00:00:00Z</updated>
    <content type="html">{esc(p['html'])}</content>
  </entry>""")
    return f"""<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>k-pdf news</title>
  <link href="{esc(site_url)}"/>
  <link rel="self" href="{esc(site_url)}feed.xml"/>
  <id>{esc(site_url)}</id>
  <updated>{updated}T00:00:00Z</updated>
{chr(10).join(entries)}
</feed>
"""


def main():
    ap = argparse.ArgumentParser(description="Builds the k-pdf website.")
    ap.add_argument("--releases", help="JSON from the GitHub releases API")
    ap.add_argument("--out", default=os.path.join(ROOT, "_site"))
    ap.add_argument("--repo", help="owner/name on GitHub")
    args = ap.parse_args()

    repo = repo_name(args.repo)
    owner, name = repo.split("/", 1)
    repo_url = f"https://github.com/{repo}"
    site_url = f"https://{owner.lower()}.github.io/{name}/"
    releases = load_releases(args.releases)
    stable = [r for r in releases if not r.get("prerelease")]
    latest = stable[0] if stable else (releases[0] if releases else None)

    posts = release_news(releases) + manual_news()
    posts.sort(key=lambda p: p["date"], reverse=True)

    # The latest version and its downloads.
    if latest:
        version = latest["tag_name"].lstrip("v")
        released = nice_date(parse_date(latest["published_at"]))
        files = downloads_of(latest)
        release_url = latest["html_url"]
        version_line = f'Version {esc(version)} · released {esc(released)}'
    else:
        version, files, release_url = "", {}, f"{repo_url}/releases/latest"
        version_line = "Get the latest version from GitHub"

    cards = []
    for key, label, _, note in PLATFORMS:
        url, file_name, size = files.get(key, (release_url, "", 0))
        detail = f"{esc(file_name)} · {size_text(size)}" if file_name else "From the Releases page"
        cards.append(f"""<a class="download-card" data-os="{key}" href="{esc(url)}">
  <span class="os-icon os-{key}" aria-hidden="true"></span>
  <span class="download-text"><strong>{label}</strong><small>{esc(note)}</small><small class="file">{detail}</small></span>
</a>""")

    # The banner on the home page points to the newest release post.
    newest_release = next((p for p in posts if p["release"]), None)
    if newest_release:
        banner = f'<a class="news-banner" href="news.html#{esc(newest_release["id"])}"><span class="tag">New</span> {esc(newest_release["title"])} <span class="muted">· {nice_date(newest_release["date"])}</span> <span aria-hidden="true">→</span></a>'
    elif posts:
        banner = f'<a class="news-banner" href="news.html#{esc(posts[0]["id"])}"><span class="tag">News</span> {esc(posts[0]["title"])} <span aria-hidden="true">→</span></a>'
    else:
        banner = ""

    news_html = "\n".join(news_post_html(p) for p in posts) or '<p class="muted">No news yet.</p>'
    recent = "\n".join(
        f'<li><a href="news.html#{esc(p["id"])}">{esc(p["title"])}</a> <time class="muted" datetime="{p["date"].isoformat()}">{nice_date(p["date"])}</time></li>'
        for p in posts[:3]
    )

    values = {
        "repo_url": repo_url,
        "release_url": esc(release_url),
        "releases_url": f"{repo_url}/releases",
        "version": esc(version),
        "version_tag": esc(f"v{version}" if version else "latest"),
        "version_line": version_line,
        "download_cards": "\n".join(cards),
        "downloads_json": esc(json.dumps({k: v[0] for k, v in files.items()})),
        "news_banner": banner,
        "news_posts": news_html,
        "recent_news": recent,
        "site_url": site_url,
        "year": str(dt.date.today().year),
    }

    layout = open(os.path.join(HERE, "layout.html"), encoding="utf-8").read()
    if os.path.isdir(args.out):
        shutil.rmtree(args.out)
    os.makedirs(args.out)

    shots = os.path.join(HERE, "static", "images")

    def shot(m):
        """{{shot:name|Alt text}}: a screenshot that follows the light or dark theme."""
        name, alt = m.group(1), m.group(2).strip()
        light = f"images/{name}-light.webp"
        dark = f"images/{name}-dark.webp"
        if not os.path.exists(os.path.join(shots, f"{name}-light.webp")):
            light = dark  # Views without a theme menu were captured dark only.
        if not os.path.exists(os.path.join(HERE, "static", dark)):
            sys.exit(f"Missing screenshot {dark}")
        return (f'<figure class="shot"><picture><source media="(prefers-color-scheme: dark)" srcset="{dark}">'
                f'<img src="{light}" alt="{esc(alt)}" width="1000" height="800" loading="lazy" decoding="async"></picture>'
                f'<figcaption>{esc(alt)}</figcaption></figure>')

    def fill(text):
        text = re.sub(r"\{\{shot:([a-z0-9-]+)\|([^}]*)\}\}", shot, text)

        def sub(m):
            key = m.group(1)
            if key not in values:
                sys.exit(f"Unknown placeholder {{{{{key}}}}}")
            return values[key]
        return re.sub(r"\{\{\s*([a-z_]+)\s*\}\}", sub, text)

    pages_dir = os.path.join(HERE, "pages")
    for page in sorted(os.listdir(pages_dir)):
        if not page.endswith(".html"):
            continue
        text = open(os.path.join(pages_dir, page), encoding="utf-8").read()
        # The first line of each page is "<!-- title | description -->".
        head, _, body = text.partition("\n")
        m = re.match(r"<!--\s*(.*?)\s*\|\s*(.*?)\s*-->", head)
        if not m:
            sys.exit(f"{page}: the first line must be <!-- title | description -->")
        title, description = m.groups()
        nav = page[:-5]
        out = layout.replace("{{content}}", body).replace("{{title}}", esc(title)).replace("{{description}}", esc(description)).replace("{{page}}", nav)
        open(os.path.join(args.out, page), "w", encoding="utf-8").write(fill(out))

    shutil.copytree(os.path.join(HERE, "static"), args.out, dirs_exist_ok=True)
    shutil.copy(os.path.join(ROOT, "packaging", "linux", "k-pdf.svg"), os.path.join(args.out, "logo.svg"))
    bench = os.path.join(ROOT, "bench", "results")
    if os.path.isdir(bench):
        os.makedirs(os.path.join(args.out, "bench"), exist_ok=True)
        for f in os.listdir(bench):
            if f.endswith(".svg"):
                shutil.copy(os.path.join(bench, f), os.path.join(args.out, "bench", f))
    open(os.path.join(args.out, "feed.xml"), "w", encoding="utf-8").write(atom_feed(posts, site_url))
    # Pages serves files as they are, without Jekyll.
    open(os.path.join(args.out, ".nojekyll"), "w").close()
    print(f"Built {args.out}: version {version or 'none'}, {len(posts)} news posts")


if __name__ == "__main__":
    main()
