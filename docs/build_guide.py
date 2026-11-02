#!/usr/bin/env python3
"""Reconstruit le guide illustré `docs/index.html` (publié sur GitHub Pages) depuis `docs/GUIDE.md`, et remet à jour le
sommaire de `GUIDE.md`. La source de vérité est GUIDE.md : on ne modifie pas index.html à la main.

    python3 docs/build_guide.py            # écrit docs/index.html et le sommaire de GUIDE.md
    python3 docs/build_guide.py --check    # échoue (code 1) si l'un des deux est périmé (CI)

Conventions de GUIDE.md :
- une section = une ancre `<a id="etape-N"></a>` puis un titre `## Étape N — titre` (ou `## titre`) ;
- `### 🖥️ Terminal (CLI)` et `### 🌐 Navigateur (Web)` forment les deux colonnes d'une étape ;
- `![légende](img/x.svg)` devient une figure avec le SVG en ligne ;
- un bloc `> ⚠️ **Attention.**` ou `> **À savoir.**` devient une boîte d'avertissement ou de note.

Dépendance : `markdown-it-py` (pip install markdown-it-py).
"""
import html
import re
import sys
from pathlib import Path

from markdown_it import MarkdownIt

DOCS = Path(__file__).resolve().parent
GUIDE = DOCS / "GUIDE.md"
INDEX = DOCS / "index.html"
CARGO = DOCS.parent / "Cargo.toml"

CLI_H = "### 🖥️ Terminal (CLI)"
WEB_H = "### 🌐 Navigateur (Web)"

md = MarkdownIt("commonmark", {"html": False, "linkify": False}).enable("table")


def version() -> str:
    m = re.search(r'^version = "([^"]+)"', CARGO.read_text(encoding="utf-8"), re.M)
    return m.group(1) if m else "?"


def strip_tags(s: str) -> str:
    return re.sub(r"<[^>]+>", "", s)


def slug_of(title: str) -> str:
    """Ancre d'une section sans `<a id>` : « Étape 8 bis-b — … » → etape-8bis-b ; autre titre → mots en minuscules."""
    m = re.match(r"Étape ([0-9]+)(?: ([a-z]+)(?:-([a-z]))?)? — ", title)
    if m:
        n, word, suffix = m.groups()
        return "etape-" + n + ("" if not word else (word if not suffix else word + "-" + suffix))
    return re.sub(r"[^a-z0-9]+", "-", html.unescape(title).lower().encode("ascii", "ignore").decode()).strip("-") or "section"


def parse_sections(text: str):
    """[(id, titre, num, corps)] ; une section commence à chaque `## ` ; son ancre est le dernier `<a id>` qui la précède
    (sinon elle est déduite du titre). num = libellé du rond (vide pour une section sans numéro)."""
    sections, anchor = [], None
    for line in text.split("\n"):
        m = re.match(r'<a id="([^"]+)"></a>\s*$', line)
        if m:
            anchor = m.group(1)
            continue
        if line.startswith("## "):
            title = line[3:].strip()
            sections.append({"id": anchor or slug_of(title), "title": title, "body": []})
            anchor = None
            continue
        if sections:
            sections[-1]["body"].append(line)
    out = []
    for s in sections:
        m = re.match(r"Étape ([0-9]+)(?: ([a-z]+)(?:-([a-z]))?)? — (.*)$", s["title"])
        body = "\n".join(s["body"]).strip("\n")
        if m:
            n, word, suffix, title = m.groups()
            out.append((s["id"], title, n + ("" if not word else f" {word}" + (f"-{suffix}" if suffix else "")), body))
        else:
            out.append((s["id"], s["title"], "", body))
    return out


def inline_svg(src: str, alt: str) -> str:
    svg = (DOCS / src).read_text(encoding="utf-8")
    svg = re.sub(r"^<\?xml[^>]*>\s*", "", svg)
    return f"<figure>{svg.strip()}\n<figcaption>{html.escape(alt)}</figcaption></figure>"


def render(chunk: str) -> str:
    """Markdown → HTML, avec les conventions du guide (images, code, boîtes)."""
    if not chunk.strip():
        return ""
    out = md.render(chunk)
    # Images seules dans un paragraphe → figure avec SVG en ligne.
    def fig(m):
        return inline_svg(m.group(2), html.unescape(m.group(1)))
    out = re.sub(r'<p><img src="([^"]*\.svg)" alt="([^"]*)"[^>]*></p>', lambda m: inline_svg(m.group(1), html.unescape(m.group(2))), out)
    out = re.sub(r'<p><img src="([^"]*)" alt="([^"]*)"[^>]*></p>', lambda m: f'<figure><img src="{m.group(1)}" alt="{m.group(2)}" loading="lazy" decoding="async"><figcaption>{m.group(2)}</figcaption></figure>', out)
    # Blocs de code → .code (le script de la page y pose le bouton « Copier »).
    out = re.sub(r"<pre><code(?: class=\"[^\"]*\")?>(.*?)</code></pre>", lambda m: f'<div class="code"><pre><code>{m.group(1)}</code></pre></div>', out, flags=re.S)
    # Citations → boîtes.
    def box(m):
        inner = m.group(1).strip()
        warn = inner.startswith("<p>⚠️")
        inner = inner.replace("<p>⚠️ ", "<p>", 1)
        lbl = re.match(r"<p><strong>([^<]*?)\.?</strong>\s*", inner)
        if lbl:
            inner = inner[: lbl.start()] + f'<p><span class="lbl">{lbl.group(1)}</span> ' + inner[lbl.end():]
        cls = "warn" if warn else "note"
        return f'<div class="box {cls}">{inner}</div>'
    out = re.sub(r"<blockquote>\s*(.*?)\s*</blockquote>", box, out, flags=re.S)
    # Tableaux défilables sur mobile.
    out = out.replace("<table>", '<div class="tw"><table>').replace("</table>", "</table></div>")
    return out


def split_columns(body: str):
    """Découpe le corps en [("md", texte)] et [("cols", cli, web)] selon les titres de colonnes."""
    lines = body.split("\n")
    blocks, buf, i = [], [], 0
    def flush():
        if buf:
            blocks.append(("md", "\n".join(buf)))
            buf.clear()
    while i < len(lines):
        if lines[i].strip() in (CLI_H, WEB_H):
            flush()
            cols = {}
            while i < len(lines) and lines[i].strip() in (CLI_H, WEB_H):
                key = "cli" if lines[i].strip() == CLI_H else "web"
                i += 1
                chunk, fenced = [], False
                # Un `#` en tête de ligne n'est un titre que hors d'un bloc de code (commentaire shell).
                while i < len(lines) and (fenced or not lines[i].startswith("#")):
                    if lines[i].lstrip().startswith("```"):
                        fenced = not fenced
                    chunk.append(lines[i])
                    i += 1
                cols[key] = "\n".join(chunk)
            blocks.append(("cols", cols.get("cli"), cols.get("web")))
            continue
        buf.append(lines[i])
        i += 1
    flush()
    return blocks


def render_section(sid: str, title: str, num: str, body: str, idx: int) -> str:
    h = f'<h2 id="h-{idx}">' + (f'<span class="num" aria-hidden="true">{html.escape(num)}</span> ' if num else "") + f"{md.renderInline(title)}</h2>"
    parts = [f'<section id="{sid}" aria-labelledby="h-{idx}">', h]
    for block in split_columns(body):
        if block[0] == "md":
            parts.append(render(block[1]))
        else:
            _, cli, web = block
            cols = []
            if cli is not None:
                cols.append('<div class="col cli"><h3><span aria-hidden="true">⌨</span> Terminal (CLI)</h3>\n' + render(cli) + "</div>")
            if web is not None:
                cols.append('<div class="col web"><h3><span aria-hidden="true">◐</span> Navigateur (Web)</h3>\n' + render(web) + "</div>")
            parts.append(f'<div class="cols{" one" if len(cols) == 1 else ""}">\n' + "\n".join(cols) + "\n</div>")
    parts.append("</section>")
    return "\n".join(p for p in parts if p)


def sommaire(sections) -> str:
    items = []
    for sid, title, num, _ in sections:
        label = f"{num}. {title}" if num else title
        items.append(f"[{label.split(' : ')[0].split(' — ')[0]}](#{sid})")
    return "**Sommaire** : " + " · ".join(items)


def build():
    guide = GUIDE.read_text(encoding="utf-8")
    sections = parse_sections(guide)
    new_guide = re.sub(r"^\*\*Sommaire\*\* : .*$", lambda _: sommaire(sections), guide, count=1, flags=re.M)
    new_guide = re.sub(r"Préversion \*\*[^*]+\*\*", f"Préversion **{version()}**", new_guide, count=1)

    old = INDEX.read_text(encoding="utf-8")
    head = old[: old.index('<nav class="toc"')]
    head = re.sub(r'<span class="badge">Préversion [^<]*</span>', f'<span class="badge">Préversion {version()}</span>', head)
    tail = old[old.index("</main>"):]
    head = head.replace("width:2.1rem;height:2.1rem;border-radius:50%", "min-width:2.1rem;height:2.1rem;padding:0 .5rem;border-radius:999px")
    if "figure img{" not in head:
        head = head.replace("</style>", "figure img{display:block;max-width:100%;height:auto;border:1px solid var(--line);border-radius:10px}\n</style>", 1)
    if ".cols.one" not in head:
        head = head.replace("</style>", "@media (min-width:56rem){.cols.one{grid-template-columns:1fr}}\n</style>", 1)
    if "a{overflow-wrap" not in head:
        head = head.replace("</style>", "a{overflow-wrap:anywhere}\n</style>", 1)
    if ".tw{" not in head:
        head = head.replace("</style>", ".tw{overflow-x:auto}table{border-collapse:collapse;margin:.8em 0;font-size:.95rem}th,td{border:1px solid var(--line);padding:6px 10px;text-align:left;vertical-align:top}th{background:var(--pan)}\n</style>", 1)
    toc = '<nav class="toc" aria-labelledby="toc-t"><h2 id="toc-t">Les étapes</h2><ol style="list-style:none;padding-left:0">' + "".join(
        f'<li><a href="#{sid}">{(html.escape(num) + ". ") if num else ""}{strip_tags(md.renderInline(title))}</a></li>' for sid, title, num, _ in sections
    ) + "</ol></nav>\n</header>\n<main id=\"contenu\" tabindex=\"-1\">\n"
    body = "\n".join(render_section(sid, title, num, b, i) for i, (sid, title, num, b) in enumerate(sections))
    index = head + toc + body + "\n" + tail
    return new_guide, index


# Documents annexes publiés en HTML (GitHub Pages sert les .md bruts) : même feuille de style que le guide.
PAGES = ["CONSTELLATIONS", "MACOS"]
LINKED = {**{f"{n}.md": f"{n}.html" for n in PAGES}, "GUIDE.md": "index.html"}


def link_md(out: str) -> str:
    """Les liens vers un document publié en HTML pointent sur sa version HTML."""
    return re.sub(r'href="([A-Za-z_]+\.md)(#[^"]*)?"', lambda m: f'href="{LINKED.get(m.group(1), m.group(1))}{m.group(2) or ""}"', out)


def build_page(name: str) -> str:
    text = (DOCS / f"{name}.md").read_text(encoding="utf-8")
    old = INDEX.read_text(encoding="utf-8")
    style = old[old.index("<style>"): old.index("</style>") + len("</style>")]
    style += "\n<style>.pg{max-width:60rem;margin:0 auto;padding:1rem 16px 3rem}</style>"
    m = re.match(r"#\s+(.*)", text)
    title = m.group(1).strip() if m else name
    body = render(re.sub(r"^#\s+.*\n", "", text, count=1))
    back = '<p><a href="index.html">← Retour au guide</a></p>'
    return link_md(f'<!doctype html>\n<html lang="fr">\n<head>\n<meta charset="utf-8">\n<meta name="viewport" content="width=device-width,initial-scale=1">\n<title>{html.escape(strip_tags(title))} — astroportd</title>\n{style}\n</head>\n<body>\n<main class="pg">\n{back}\n<h1>{md.renderInline(title)}</h1>\n{body}\n{back}\n</main>\n</body>\n</html>\n')


def main():
    new_guide, index = build()
    index = link_md(index)
    pages = {n: build_page(n) for n in PAGES}
    if "--check" in sys.argv:
        targets = [("GUIDE.md", GUIDE, new_guide), ("index.html", INDEX, index)] + [(f"{n}.html", DOCS / f"{n}.html", v) for n, v in pages.items()]
        stale = [n for n, p, v in targets if not p.exists() or p.read_text(encoding="utf-8") != v]
        if stale:
            print("périmé : " + ", ".join(stale) + " — relancez python3 docs/build_guide.py", file=sys.stderr)
            sys.exit(1)
        print("guide à jour")
        return
    GUIDE.write_text(new_guide, encoding="utf-8")
    INDEX.write_text(index, encoding="utf-8")
    for n, v in pages.items():
        (DOCS / f"{n}.html").write_text(v, encoding="utf-8")
    print(f"docs/index.html et le sommaire de GUIDE.md reconstruits ({len(parse_sections(new_guide))} sections, version {version()})")


if __name__ == "__main__":
    main()
