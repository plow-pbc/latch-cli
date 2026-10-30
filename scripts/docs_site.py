#!/usr/bin/env python3
"""Latch's docs site: the Markdown pages in docs/site, rendered as plow.co
pages for plow.co/latch/docs. commands.md is written from `latch help
--json`, so the Commands page says what the binary does.

    docs_site.py write --latch LATCH --out DIR   commands.md from LATCH, then every page into DIR
    docs_site.py check --latch LATCH             fails if commands.md is not what LATCH says
    docs_site.py publish --latch LATCH [PLOW] [--dry-run]
                                                 every page into plow's plow.co/latch/docs, as a plow PR

The Markdown is the small subset the pages use: headings, paragraphs, lists,
tables, quotes, fenced code and inline code, bold and links. Anything else
is refused rather than shown wrong.
"""

import argparse
import html
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "docs" / "site"
PLOW = "plow-pbc/plow"
# Where the pages live in a plow checkout.
PLOW_DOCS = "plow.co/latch/docs"
BASE = "/latch/docs"
SOURCE = "https://github.com/plow-pbc/latch-cli/blob/main/docs/site"
# The pages, in the order the sidebar lists them.
PAGES = ["index", "connect-an-agent", "how-latch-works", "troubleshooting", "commands", "security"]

# Where each command goes on the Commands page, by the first word of its id.
GROUPS = [
    ("Set up", ["setup", "login", "logout", "status", "devices", "hosts", "update", "app", "help", "skill"]),
    ("Run a host", ["serve", "stop", "dashboard", "logs", "settings"]),
    ("Agents", ["agents"]),
    ("Programs and files", ["exec", "file"]),
    ("Browser", ["browser"]),
    ("Vault", ["vault"]),
    ("Rules", ["rules"]),
    ("Connectors", ["plugins", "google", "messages", "contacts"]),
    ("Audit", ["audit"]),
]

COMMANDS_INTRO = """\
# Commands

Every command prints one JSON object. Add `--pretty` to read it yourself.
`latch help COMMAND` lists a command's options, and
`latch help COMMAND --examples` shows how to call it.

| For | Means |
|---|---|
| Host | You, the owner, run it. |
| Agent | Your agent runs it, to ask a host for something. |

Commands marked "Owner only" work from your own computers, never for an
agent.

## Choosing a host

`--host NAME` sends an Agent command to that host, over the encrypted
connection between the two computers. Without it, the command goes to the
host `LATCH_HOST` names; else to this computer, if it hosts; else to your only
host. With several hosts and none named, it stops with `host_required`.
`--host default` is the host `latch hosts default NAME` saved on this
computer, or for this agent.

`rules`, `vault` and `audit` work on this computer's own host, or on another
of your hosts with `--host NAME`. Bare `latch status` reports this computer
and ignores `LATCH_HOST`; `latch status --host NAME` reports another host, for
you only, never for an agent. `latch browser setup` runs only on the computer
you're at: with `--host` it stops with `host_not_supported`.

```sh host
latch rules show --host my-pi
```

```sh agent
latch browser open --host my-mac https://example.com
```
"""

# Who runs a command, from what `latch help --json` says of it: Agent for a
# request an agent may send to a host, Host for everything you, the owner, run,
# and both for one an agent also runs on its own computer, or one that works on
# your own host and an agent sends with --host.
# Agents also run this one, though it doesn't go to a host.
AGENTS_RUN = {"skill"}


def pill(action):
    if action["agent_on_own_computer"] or (action["runs_on"] == "either" and not action["owner_only"]):
        return "Host Agent"
    agent = action["runs_on"] == "host" and not action["owner_only"]
    return "Agent" if agent or action["id"] in AGENTS_RUN else "Host"


def url(page):
    return BASE if page == "index" else f"{BASE}/{page}"


# --- Commands page, from `latch help --json` ---------------------------------

def commands_markdown(help_json):
    actions = help_json["actions"]
    placed = {word for _, words in GROUPS for word in words}
    unplaced = sorted({a["id"].split(".")[0] for a in actions} - placed)
    if unplaced:
        raise SystemExit(f"docs_site.py: no group for {', '.join(unplaced)}: add them to GROUPS")
    out = [COMMANDS_INTRO]
    for title, words in GROUPS:
        rows = []
        # In the group's order of commands, each command's actions as help lists them.
        grouped = sorted((a for a in actions if a["id"].split(".")[0] in words),
                         key=lambda a: words.index(a["id"].split(".")[0]))
        for action in grouped:
            if action["id"].startswith("google."):
                continue
            command = "latch " + action["id"].replace(".", " ")
            what = action["summary"].rstrip(".") + "."
            if action["owner_only"]:
                what += " Owner only."
            if action["id"] == "google":
                allowed = [a["id"].split(".")[1:] for a in actions if a["id"].startswith("google.")]
                command = "latch google -- COMMAND"
                what = "Run one allowed command on a host's Google account."
                for service, name in (("calendar", "Calendar"), ("gmail", "Gmail")):
                    words = [" ".join(rest) for first, *rest in allowed if first == service]
                    what += f" {name}: " + ", ".join(f"`{w}`" for w in words) + "."
            # The command's first example, with what it does, when it shows more
            # than the bare command.
            first = action["examples"][0]
            example = ""
            if first["command"] != command:
                label = first["what"] if first["what"].endswith(("?", ".")) else first["what"] + ":"
                example = f"<br>{cell(label)} `{cell(first['command'])}`"
            rows.append(f"| `{command}` | {pill(action)} | {cell(what)}{example} |")
        out.append(f"## {title}\n\n| Command | For | What it does, and an example |\n|---|---|---|\n"
                   + "\n".join(rows) + "\n")
    return "\n".join(out)


def cell(text):
    return text.replace("|", "\\|")


def help_json(latch):
    answer = subprocess.run([latch, "help", "--json"], capture_output=True, text=True, check=True,
                            env={"PATH": "/usr/bin:/bin", "HOME": tempfile.gettempdir()})
    return json.loads(answer.stdout)


# --- Markdown to HTML ----------------------------------------------------------

def slug(text):
    return re.sub(r"[^a-z0-9]+", "-", re.sub(r"<[^>]+>", "", text).lower()).strip("-")


def link(target):
    """A page link (`connect-an-agent.md#x`) as its site URL; others as written."""
    m = re.fullmatch(r"([a-z-]+)\.md(#.*)?", target)
    if m and m.group(1) in PAGES:
        return url(m.group(1)) + (m.group(2) or "")
    if re.match(r"https?://|#|/", target):
        return target
    raise SystemExit(f"docs_site.py: link to {target!r} is neither a page nor a URL")


def inline(text):
    """Inline Markdown: code spans first, so nothing inside them is read."""
    parts = re.split(r"(`[^`]+`)", text)
    out = []
    for part in parts:
        if part.startswith("`") and part.endswith("`") and len(part) > 1:
            code = part[1:-1]
            # A word or a short phrase (`--pretty`, `--host NAME`) never breaks at
            # its hyphens or slashes.
            tag = '<code class="word">' if " " not in code or len(code) <= 24 else "<code>"
            out.append(f"{tag}{html.escape(code, quote=False)}</code>")
            continue
        part = html.escape(part, quote=False)
        part = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", part)
        part = re.sub(r"(?<![\w*])\*(?!\s)(.+?)(?<!\s)\*(?![\w*])", r"<em>\1</em>", part)
        part = re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)",
                      lambda m: f'<a href="{html.escape(link(m.group(2)))}">{m.group(1)}</a>', part)
        out.append(part)
    return "".join(out)


def highlight(line):
    """A shell line: the program in volt, a trailing comment dimmed."""
    code, comment = line, ""
    m = re.search(r"(^|\s)(#\s.*)$", line)
    if m:
        code, comment = line[: m.start(2)], line[m.start(2):]
    m = re.match(r"(\s*)(export|[a-z][\w.-]*)(\b.*)", code, re.S)
    code = (f"{m.group(1)}<span class=\"k\">{html.escape(m.group(2))}</span>{html.escape(m.group(3), quote=False)}"
            if m else html.escape(code, quote=False))
    return code + (f'<span class="c">{html.escape(comment, quote=False)}</span>' if comment else "")


PILLS = {"host": "Host", "agent": "Agent"}


def pill_html(name):
    return f'<span class="pill pill-{name.lower()}">{html.escape(name)}</span>'


def code_block(info, lines):
    lang, *tags = info.split()
    # A command block says who runs it: host or agent, or both where the
    # same command works on either (a check, an update), and nowhere else.
    if lang == "sh" and tags not in (["host"], ["agent"], ["host", "agent"]):
        raise SystemExit(f"docs_site.py: a sh block needs host, agent, or host agent: {info!r}")
    if lang != "sh" and tags:
        raise SystemExit(f"docs_site.py: only a sh block takes a tag: {info!r}")
    body = "\n".join(highlight(line) for line in lines) if lang == "sh" else html.escape("\n".join(lines), quote=False)
    label = {"sh": "terminal", "json": "output", "text": "output"}.get(lang)
    if label is None:
        raise SystemExit(f"docs_site.py: code block language {lang!r} is not sh, json or text")
    copy = ('<button class="code-copy" type="button" aria-label="Copy code"><i class="ph-bold ph-copy"></i></button>'
            if lang == "sh" else "")
    panel = (f'<div class="code" data-lang="{lang}"><div class="code-bar"><span class="fn">{label}</span>{copy}</div>'
             f"<pre><code>{body}</code></pre></div>")
    if not tags:
        return panel
    tabs = "".join(f'<span class="code-tab pill-{t}">{PILLS[t]}</span>' for t in tags)
    return f'<div class="code-block"><div class="code-tabs">{tabs}</div>{panel}</div>'


def table(lines):
    def cells(line):
        return [c.strip().replace("\\|", "|") for c in re.split(r"(?<!\\)\|", line.strip())[1:-1]]
    head, rows = cells(lines[0]), [cells(line) for line in lines[2:]]
    if not re.fullmatch(r"\|(\s*:?-+:?\s*\|)+", lines[1].strip()):
        raise SystemExit(f"docs_site.py: a table's second line must be its divider: {lines[1]!r}")
    def td(c, column):
        if head[column] == "For":
            if c and c not in ("Host", "Agent", "Host Agent"):
                raise SystemExit(f"docs_site.py: a For cell is not Host, Agent or Host Agent: {c!r}")
            return " ".join(pill_html(name) for name in c.split())
        # A cell's lines, as `<br>` splits them on GitHub too.
        return "<br>".join(inline(part) for part in c.split("<br>"))
    th = "".join(f"<th>{inline(c)}</th>" for c in head)
    # Each cell carries its column's name, which a phone shows above it.
    labels = [html.escape(re.sub(r"[`*]", "", c)) for c in head]
    trs = "".join("<tr>" + "".join(f'<td data-label="{label}">{td(c, i)}</td>'
                                   for i, (label, c) in enumerate(zip(labels, row)))
                  + "</tr>" for row in rows)
    return f'<div class="table-wrap"><table><thead><tr>{th}</tr></thead><tbody>{trs}</tbody></table></div>'


def render(markdown):
    """The page's title and its body as HTML."""
    titles = []
    body = blocks(markdown.splitlines(), titles)
    if len(titles) != 1:
        raise SystemExit("docs_site.py: a page needs one # title")
    return titles[0], body


def blocks(lines, titles):
    """Blocks of Markdown as HTML."""
    out, i = [], 0
    while i < len(lines):
        line = lines[i]
        if not line.strip():
            i += 1
        elif line.startswith("```"):
            lang, end = line[3:].strip(), i + 1
            while end < len(lines) and lines[end] != "```":
                end += 1
            if end == len(lines):
                raise SystemExit("docs_site.py: a code block is not closed")
            out.append(code_block(lang, lines[i + 1:end]))
            i = end + 1
        elif m := re.match(r"(#{1,3}) (.+)", line):
            level, text = len(m.group(1)), m.group(2)
            if level == 1:
                titles.append(text)
                out.append(f"<h1>{inline(text)}</h1>")
            else:
                out.append(f'<h{level} id="{slug(text)}">{inline(text)}</h{level}>')
            i += 1
        elif line.startswith("|"):
            end = i
            while end < len(lines) and lines[end].startswith("|"):
                end += 1
            out.append(table(lines[i:end]))
            i = end
        elif line.startswith("> "):
            end = i
            while end < len(lines) and lines[end].startswith(">"):
                end += 1
            text = " ".join(l[1:].strip() for l in lines[i:end])
            out.append(f'<div class="note"><p>{inline(text)}</p></div>')
            i = end
        elif re.match(r"(- |\d+\. )", line):
            ordered = not line.startswith("- ")
            items = []
            while i < len(lines) and (re.match(r"(- |\d+\. )", lines[i]) or lines[i].startswith("  ")):
                if lines[i].startswith("  "):
                    items[-1] += " " + lines[i].strip()
                else:
                    items.append(re.sub(r"^(- |\d+\. )", "", lines[i]))
                i += 1
            tag = "ol" if ordered else "ul"
            out.append(f"<{tag}>" + "".join(f"<li>{inline(item)}</li>" for item in items) + f"</{tag}>")
        else:
            end = i
            while end < len(lines) and lines[end].strip() and not re.match(r"(```|#|\||> |- |\d+\. )", lines[end]):
                end += 1
            out.append(f"<p>{inline(' '.join(l.strip() for l in lines[i:end]))}</p>")
            i = end
    return "\n".join(out)


def description(body):
    first = re.search(r"<p>(.*?)</p>", body, re.S)
    text = re.sub(r"<[^>]+>", "", first.group(1)) if first else ""
    return html.unescape(text).split(". ")[0].rstrip(".") + "."


def site(out):
    template = (SITE / "page.html").read_text()
    pages = {}
    for page in PAGES:
        pages[page] = render((SITE / f"{page}.md").read_text())
    for page, (title, body) in pages.items():
        nav = "\n".join(
            f'<a href="{url(p)}"' + (' aria-current="page"' if p == page else "") + f">{html.escape(pages[p][0])}</a>"
            for p in PAGES)
        text = template
        for key, value in {
            "title": html.escape(title),
            "description": html.escape(description(body)),
            "canonical": "https://plow.co" + url(page),
            "nav": nav,
            "content": body,
            "source": f"{SOURCE}/{page}.md",
        }.items():
            text = text.replace("{{" + key + "}}", value)
        if "{{" in text:
            raise SystemExit(f"docs_site.py: page.html has a placeholder nothing fills: {re.search(r'{{.*?}}', text).group()}")
        (out / f"{page}.html").write_text(text)


# --- Publishing into plow -----------------------------------------------------

def git(where, *args):
    return subprocess.run(["git", "-C", str(where), *args], check=True, capture_output=True, text=True).stdout.strip()


def publish(plow, dry_run):
    """Every page into a plow checkout of origin/main, committed on a new
    branch and opened as a plow PR. Without PLOW, a fresh shallow clone."""
    sha = git(ROOT, "rev-parse", "--short", "HEAD")
    # The PR says which latch the pages came from, so they must be that commit's.
    if not dry_run and git(ROOT, "status", "--porcelain", "--", "docs/site", "scripts/docs_site.py"):
        raise SystemExit("docs_site.py: docs/site or scripts/docs_site.py has uncommitted changes: commit them first")
    with tempfile.TemporaryDirectory(prefix="latch-docs-") as temp:
        work = Path(temp) / "plow"
        if plow:
            if git(plow, "status", "--porcelain"):
                raise SystemExit(f"docs_site.py: {plow} has uncommitted changes")
            git(plow, "fetch", "--quiet", "origin", "main")
            git(plow, "worktree", "add", "--quiet", "--detach", str(work), "FETCH_HEAD")
        else:
            subprocess.run(["gh", "repo", "clone", PLOW, str(work), "--", "--depth", "1", "--quiet"], check=True)
        try:
            return commit_and_open(work, sha, dry_run)
        finally:
            if plow:
                git(plow, "worktree", "remove", "--force", str(work))


def commit_and_open(work, sha, dry_run):
    docs = work / PLOW_DOCS
    # A page dropped from PAGES goes from plow too.
    for old in docs.glob("*.html"):
        old.unlink()
    docs.mkdir(parents=True, exist_ok=True)
    site(docs)
    git(work, "add", "--all", "--", PLOW_DOCS)
    if subprocess.run(["git", "-C", str(work), "diff", "--cached", "--quiet"]).returncode == 0:
        print(f"docs_site.py: {PLOW_DOCS} already matches latch {sha}: no PR")
        return 0
    print(git(work, "diff", "--cached", "--stat"))
    if dry_run:
        print("docs_site.py: dry run: nothing committed or pushed")
        return 0
    branch = f"latch-docs-{sha}"
    title = f"{PLOW_DOCS}: update from latch {sha}"
    full = git(ROOT, "rev-parse", "HEAD")
    body = (f"The pages of plow.co/latch/docs, rendered from latch-cli's `docs/site` at "
            f"[{sha}](https://github.com/plow-pbc/latch-cli/tree/{full}/docs/site) by `just docs-publish`. "
            f"Edit them there, not here.")
    git(work, "commit", "--quiet", "-m", title)
    git(work, "push", "--quiet", "origin", f"HEAD:refs/heads/{branch}")
    pr = subprocess.run(["gh", "pr", "create", "--base", "main", "--head", branch, "--title", title, "--body", body],
                        cwd=work, check=True, capture_output=True, text=True).stdout.strip()
    print(pr)
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("mode", choices=["write", "check", "publish"])
    parser.add_argument("plow", nargs="?", type=Path,
                        help="with publish: a clean plow checkout (default: a fresh clone of plow-pbc/plow)")
    parser.add_argument("--latch", required=True, help="the latch binary whose help the Commands page shows")
    parser.add_argument("--out", type=Path, help="with write: where the HTML pages go")
    parser.add_argument("--dry-run", action="store_true",
                        help="with publish: stop before committing and print what would change")
    args = parser.parse_intermixed_args()
    if args.mode != "publish" and (args.plow or args.dry_run):
        parser.error("a plow checkout and --dry-run go with publish")
    commands = commands_markdown(help_json(args.latch))
    if args.mode == "write":
        if not args.out:
            parser.error("write needs --out")
        (SITE / "commands.md").write_text(commands)
        args.out.mkdir(parents=True, exist_ok=True)
        site(args.out)
        print(f"docs_site.py: wrote {len(PAGES)} pages into {args.out}")
        return 0
    if (SITE / "commands.md").read_text() != commands:
        print("docs_site.py: docs/site/commands.md is not what `latch help --json` says: "
              "run `just docs-site OUT` and commit it", file=sys.stderr)
        return 1
    if args.mode == "publish":
        return publish(args.plow, args.dry_run)
    with tempfile.TemporaryDirectory() as out:
        site(Path(out))
    print("docs_site.py: commands.md is current and every page renders")
    return 0


if __name__ == "__main__":
    sys.exit(main())
