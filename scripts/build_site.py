#!/usr/bin/env python3
"""Builds the Phase / LUCID GitHub Pages site into _site/.

    python3 scripts/build_site.py           # build and verify into _site/
    python3 -m http.server -d _site 8090    # preview at http://localhost:8090

Sources, all in site/:
- pages/*.html          Hand-written page fragments; leading <!-- key: value --> sets title and description.
- help/registry.json    Single source of truth for help sidebar, search index, sitemap, and llms.txt.
- help/<slug>.md        GitHub-flavoured Markdown rendered via pandoc (including GFM alerts).
- static/               Stylesheet, search/theme script, and icons.
- layout.html           The single page shell.

The build fails closed and writes nothing if:
- any registry entry lacks site/help/<slug>.md, or an unindexed .md exists in site/help/
- any help page's leading # Title differs from registry.json
- any internal href or #anchor fails to resolve
- any IPv4 address outside RFC 5737 / loopback documentation ranges appears in built pages
- any private local path or private fleet identifier appears in built pages
"""
import html
import json
import os
import re
import shutil
import subprocess
import sys
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
OUT = ROOT / "_site"
BASE_URL = os.environ.get("PHASE_SITE_BASE_URL", "https://msitarzewski.github.io/phase/").rstrip("/") + "/"
REPO = "https://github.com/msitarzewski/phase"

NAV = [
    ("index.html", "Overview"),
    ("releases.html", "Download"),
    ("help/index.html", "Help"),
    ("help/http-api.html", "APIs & Protocols"),
    ("security.html", "Security"),
]
FOOTER = [
    ("index.html", "Overview"),
    ("releases.html", "Download & Releases"),
    ("why.html", "Why Phase"),
    ("help/index.html", "Help"),
    ("help/http-api.html", "HTTP API"),
    ("help/protocols.html", "Protocols"),
    ("security.html", "Security"),
    (REPO, "Source"),
]
DOCUMENTATION_RANGES = (
    "192.0.2.",
    "198.51.100.",
    "203.0.113.",
    "127.0.0.1",
    "0.0.0.0",
    "255.255.255.",
    "1.1.1.1",
)
OPSEC_PATTERNS = [
    (r"/Users/[A-Za-z0-9._-]+", "local macOS user path"),
    (r"100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d+\.\d+", "CGNAT/Tailscale IP"),
    (r"192\.168\.\d+\.\d+", "RFC 1918 LAN IP"),
]

errors = []


def fail(message: str) -> None:
    errors.append(message)


def front_matter(text: str):
    match = re.match(r"\s*<!--(.*?)-->", text, re.S)
    meta = {}
    if match:
        for line in match.group(1).strip().splitlines():
            key, _, value = line.partition(":")
            meta[key.strip()] = value.strip()
        text = text[match.end():]
    return meta, text


def pandoc(markdown_text: str) -> str:
    return subprocess.run(
        ["pandoc", "--from=gfm+alerts", "--to=html5", "--wrap=none"],
        input=markdown_text,
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def plain(fragment: str) -> str:
    fragment = re.sub(r'<a class="anchor"[^>]*>#</a>', "", fragment)
    text = re.sub(r"<[^>]+>", " ", fragment)
    return re.sub(r"\s+", " ", html.unescape(text)).strip()


def nav_html(current: str, root: str) -> str:
    items = []
    api_slugs = {"help/http-api.html", "help/cli-and-env.html", "help/protocols.html", "help/cryptography-and-receipts.html", "help/architecture-and-crates.html"}
    for target, label in NAV:
        is_here = (
            target == current
            or (target == "help/http-api.html" and current in api_slugs)
            or (target == "help/index.html" and current.startswith("help/") and current not in api_slugs)
        )
        here = ' aria-current="page"' if is_here else ""
        items.append(f'      <a href="{root}{target}"{here}>{html.escape(label)}</a>')
    return "\n".join(items)


def footer_html(root: str) -> str:
    links = []
    for target, label in FOOTER:
        href = target if target.startswith("http") else root + target
        links.append(f'    <a href="{href}">{html.escape(label)}</a>')
    return "\n".join(links)


def render(layout: str, output: str, *, title: str, description: str, body: str, og_type: str = "website", jsonld=None) -> str:
    depth = output.count("/")
    root = "../" * depth
    canonical = BASE_URL + ("" if output == "index.html" else output)
    graph = jsonld or {
        "@context": "https://schema.org",
        "@type": "WebPage",
        "name": title,
        "description": description,
        "url": canonical,
    }
    page = (
        layout.replace("{{title}}", html.escape(title))
        .replace("{{og_title}}", html.escape(title))
        .replace("{{description}}", html.escape(description, quote=True))
        .replace("{{canonical}}", canonical)
        .replace("{{base_url}}", BASE_URL)
        .replace("{{og_type}}", og_type)
        .replace("{{jsonld}}", json.dumps(graph, ensure_ascii=False).replace("</", "<\\/"))
        .replace("{{nav}}", nav_html(output, root))
        .replace("{{footer}}", footer_html(root))
        .replace("{{body}}", body)
        .replace("{{root}}", root)
    )
    path = OUT / output
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(page)
    return page


def link_help(fragment: str) -> str:
    """Convert intra-help `.md` links to `.html` and attach section anchors to h2/h3."""
    fragment = re.sub(
        r'href="([a-z0-9-]+)\.md(#[^"]*)?"',
        lambda m: f'href="{m.group(1)}.html{m.group(2) or ""}"',
        fragment,
    )
    fragment = re.sub(
        r'<(h[23]) id="([^"]+)">(.*?)</\1>',
        lambda m: f'<{m.group(1)} id="{m.group(2)}">{m.group(3)}<a class="anchor" href="#{m.group(2)}" aria-label="Link to this section">#</a></{m.group(1)}>',
        fragment,
    )
    return fragment


def build_help(layout: str, registry: dict):
    pages = [(group["name"], page) for group in registry["groups"] for page in group["pages"]]
    slugs = [page["slug"] for _, page in pages]
    on_disk = {p.stem for p in (SITE / "help").glob("*.md")}
    for missing in sorted(set(slugs) - on_disk):
        fail(f"help: registry names '{missing}', which has no site/help/{missing}.md")
    for extra in sorted(on_disk - set(slugs)):
        fail(f"help: site/help/{extra}.md is not in site/help/registry.json")

    index = []
    sidebar_groups = []
    for group in registry["groups"]:
        items = "".join(
            f'<li><a href="{{ROOTH}}{p["slug"]}.html" data-slug="{p["slug"]}">{html.escape(p["title"])}</a></li>'
            for p in group["pages"]
        )
        sidebar_groups.append(f'<h4>{html.escape(group["name"])}</h4><ul>{items}</ul>')
    sidebar = "".join(sidebar_groups)

    for position, (group, page) in enumerate(pages):
        slug = page["slug"]
        source = SITE / "help" / f"{slug}.md"
        if not source.exists():
            continue
        text = source.read_text()
        heading = re.match(r"#\s+(.+)", text)
        if not heading or heading.group(1).strip() != page["title"]:
            found = heading.group(1).strip() if heading else "<none>"
            fail(f"help/{slug}: leading H1 '{found}' differs from registry title '{page['title']}'")
        body = link_help(pandoc(text))

        for section in re.split(r"(?=<h2 id=)", body):
            match = re.match(r'<h2 id="([^"]+)">(.*?)<a class="anchor"', section)
            entry = {"title": page["title"], "group": group, "body": plain(section)[:2000]}
            if match:
                entry.update(heading=plain(match.group(2)), url=f"help/{slug}.html#{match.group(1)}")
            else:
                entry.update(url=f"help/{slug}.html")
            index.append(entry)

        previous = pages[position - 1][1] if position > 0 else None
        following = pages[position + 1][1] if position + 1 < len(pages) else None
        pager = '<nav class="pager" aria-label="Previous and next">'
        pager += (
            f'<a class="prev" href="{previous["slug"]}.html"><span>← Previous</span>{html.escape(previous["title"])}</a>'
            if previous
            else "<span></span>"
        )
        pager += (
            f'<a class="next" href="{following["slug"]}.html"><span>Next →</span>{html.escape(following["title"])}</a>'
            if following
            else ""
        )
        pager += "</nav>"

        side = sidebar.replace("{ROOTH}", "").replace(
            f'data-slug="{slug}"', f'data-slug="{slug}" aria-current="page"'
        )
        main = (
            f'<div class="docs"><aside class="sidebar" aria-label="Documentation">{side}</aside>'
            f'<main class="doc" id="content">'
            f'<details class="mobile-nav"><summary>Browse documentation</summary>'
            f'<div class="sidebar-body sidebar" style="display:block;position:static;height:auto;width:auto">{side}</div></details>'
            f'<div class="crumbs"><a href="index.html">Help</a> / {html.escape(group)}</div>'
            f'<article class="prose">{body}</article><hr class="rule">{pager}'
            f'<p class="contrib">Written directly from the Phase &amp; LUCID source code. '
            f'Spot an inaccuracy? <a href="{REPO}/blob/main/site/help/{slug}.md">Edit or report it on GitHub</a>.</p>'
            f"</main></div>"
        )
        jsonld = {
            "@context": "https://schema.org",
            "@graph": [
                {
                    "@type": "TechArticle",
                    "headline": page["title"],
                    "description": page["summary"],
                    "url": f"{BASE_URL}help/{slug}.html",
                    "inLanguage": "en",
                    "about": {
                        "@type": "SoftwareApplication",
                        "name": "LUCID (Phase)",
                        "operatingSystem": "macOS, Linux, Windows",
                        "applicationCategory": "DeveloperApplication",
                    },
                },
                {
                    "@type": "BreadcrumbList",
                    "itemListElement": [
                        {"@type": "ListItem", "position": 1, "name": "Help", "item": f"{BASE_URL}help/"},
                        {"@type": "ListItem", "position": 2, "name": page["title"], "item": f"{BASE_URL}help/{slug}.html"},
                    ],
                },
            ],
        }
        render(
            layout,
            f"help/{slug}.html",
            title=f'{page["title"]} · Phase & LUCID Help',
            description=page["summary"],
            body=main,
            og_type="article",
            jsonld=jsonld,
        )

    hub = [
        '<div class="docs"><aside class="sidebar" aria-label="Documentation">' + sidebar.replace("{ROOTH}", "") + "</aside>",
        '<main class="doc prose" id="content"><h1>Help &amp; Technical Reference</h1>'
        '<p class="doc-summary">Plain-English setup guides at the top, full HTTP API, CLI, and libp2p protocol specifications underneath. '
        "Press <kbd>⌘K</kbd> or <kbd>/</kbd> to search every section.</p>",
    ]
    for group in registry["groups"]:
        hub.append(f'<div class="hub-group"><h2>{html.escape(group["name"])}</h2><div class="grid">')
        for p in group["pages"]:
            hub.append(
                f'<a class="card" href="{p["slug"]}.html">'
                f'<h3>{html.escape(p["title"])}</h3>'
                f'<p>{html.escape(p["summary"])}</p></a>'
            )
        hub.append("</div></div>")
    hub.append("</main></div>")
    render(
        layout,
        "help/index.html",
        title="Help & Reference · Phase & LUCID",
        description="Setup tutorials, operator policy, GPU pooling, HTTP APIs, CLI flags, and Phase libp2p protocol specs.",
        body="".join(hub),
    )
    (OUT / "help" / "search-index.json").write_text(json.dumps(index, ensure_ascii=False))
    return pages


def check_links() -> None:
    """Every internal href resolves to a built file, and every #anchor matches an id."""
    ids = {}
    for page in OUT.rglob("*.html"):
        ids[page] = set(re.findall(r'id="([^"]+)"', page.read_text()))
    for page in OUT.rglob("*.html"):
        for href in re.findall(r'href="([^"]+)"', page.read_text()):
            if re.match(r"[a-z]+:|//", href) or href.startswith("mailto:"):
                continue
            target, _, anchor = href.partition("#")
            path = (page.parent / target).resolve() if target else page
            if target.endswith("/"):
                path = path / "index.html"
            if not path.exists():
                fail(f"{page.relative_to(OUT)}: broken link -> {href}")
            elif anchor and path.suffix == ".html" and anchor not in ids.get(path, set()):
                fail(f"{page.relative_to(OUT)}: missing anchor #{anchor} in {target or 'same page'}")


def check_addresses_and_opsec() -> None:
    """Fail if non-documentation IPv4s or private local identifiers appear in built pages."""
    for page in OUT.rglob("*.html"):
        raw = page.read_text()
        text = plain(raw)
        for address in re.findall(r"(?<![\d.])(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})(?![\d.])", text):
            # Ignore semantic version strings like 10.0.28000.2705 (segments > 255)
            octets = [int(x) for x in address.split(".")]
            if any(o > 255 for o in octets):
                continue
            if not address.startswith(DOCUMENTATION_RANGES):
                fail(f"{page.relative_to(OUT)}: IPv4 address {address} is outside RFC 5737 / loopback documentation ranges")
        for pat, label in OPSEC_PATTERNS:
            matches = re.findall(pat, raw, flags=re.IGNORECASE)
            if matches:
                fail(f"{page.relative_to(OUT)}: opsec check flagged {label}: {matches[:3]}")


def main() -> None:
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir()
    shutil.copytree(SITE / "static", OUT / "static")
    (OUT / ".nojekyll").write_text("")
    layout = (SITE / "layout.html").read_text()

    outputs = []
    for fragment in sorted((SITE / "pages").glob("*.html")):
        meta, body = front_matter(fragment.read_text())
        output = fragment.name
        title = meta.get("title", "Phase & LUCID")
        description = meta.get("description", "")
        jsonld = None
        if output == "index.html":
            jsonld = {
                "@context": "https://schema.org",
                "@type": "SoftwareApplication",
                "name": "LUCID (Phase)",
                "operatingSystem": "macOS, Linux, Windows",
                "applicationCategory": "DeveloperApplication",
                "description": description,
                "url": BASE_URL,
                "license": "https://www.gnu.org/licenses/agpl-3.0.html",
            }
        render(layout, output, title=title, description=description, body=body, jsonld=jsonld)
        outputs.append((output, title, description))

    registry = json.loads((SITE / "help" / "registry.json").read_text())
    help_pages = build_help(layout, registry)

    urls = (
        [BASE_URL]
        + [BASE_URL + o for o, _, _ in outputs if o != "index.html"]
        + [BASE_URL + "help/"]
        + [f"{BASE_URL}help/{p['slug']}.html" for _, p in help_pages]
    )
    today = date.today().isoformat()
    (OUT / "sitemap.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
        + "".join(f"  <url><loc>{u}</loc><lastmod>{today}</lastmod></url>\n" for u in urls)
        + "</urlset>\n"
    )
    (OUT / "robots.txt").write_text(f"User-agent: *\nAllow: /\nSitemap: {BASE_URL}sitemap.xml\n")

    llms = [
        "# Phase & LUCID",
        "",
        "> Open verifiable compute substrate (Apache-2.0) and Ollama-compatible peer-to-peer GPU inference daemon + Desktop app (AGPL-3.0-or-later). No accounts, no billing, no lock-in.",
        "",
        "## Pages",
        "",
    ]
    llms += [f"- [{t}]({BASE_URL}{'' if o == 'index.html' else o}): {d}" for o, t, d in outputs]
    group_name = None
    for name, page in help_pages:
        if name != group_name:
            llms += ["", f"## Help: {name}", ""]
            group_name = name
        llms.append(f"- [{page['title']}]({BASE_URL}help/{page['slug']}.html): {page['summary']}")
    (OUT / "llms.txt").write_text("\n".join(llms) + "\n")

    check_links()
    check_addresses_and_opsec()
    if errors:
        print("build_site: the site was NOT built:", file=sys.stderr)
        for message in errors:
            print(f"  - {message}", file=sys.stderr)
        shutil.rmtree(OUT)
        sys.exit(1)

    print(f"build_site: {len(outputs)} top-level pages, {len(help_pages)} help/reference pages -> {OUT.relative_to(ROOT)}/")


if __name__ == "__main__":
    main()
