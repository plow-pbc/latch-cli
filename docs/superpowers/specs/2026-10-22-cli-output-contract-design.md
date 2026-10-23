# CLI output contract and copy pass

Date: 2026-10-22. Status: approved direction (chef: "go"); revised after first read and a check against the code at origin/main f626899, ready for review.
Product stage: internal tool, pre-PMF. Primary users are agents and a future GUI running latch as a subprocess; humans use it mainly for setup and config. No back-compat with today's output is required.

## 0. In one paragraph

Two things. First, **consistent JSON**: every regular command returns JSON on stdout and fails with one JSON error shape on stderr, whether or not it runs in a terminal, so an agent or GUI can rely on a single contract. Second, **a copy pass**: every string a human reads (setup and login screens, error messages, help one-liners) is rewritten to a short, plain house style, proposed as a table first and applied after the chef approves it.

## 1. Why

Today latch mixes two rules: stdout is JSON for most commands, but errors become plain sentences when stderr is a terminal, some commands print text even when piped (`setup`, `logs`, `status --pretty`), and nothing forces JSON. A GUI or agent cannot rely on one contract, and human copy is verbose. Research on best-in-class CLIs (`research/research-cli-output-claude.md`, `research/research-cli-output-codex.md`, next to this spec) found that the consistent ones use one global switch and never let terminal detection decide the data format.

## 2. The contract

One sentence: **every command returns JSON, except the human commands (`setup`, interactive `login`, the dashboard) and help text.** (Chef, 2026-10-22: no `--human` flag and no per-command text screens.)

### 2.1 JSON by default, everywhere
- Every command writes its result as JSON on stdout and any error as JSON on stderr, whether or not stdout/stderr is a terminal. No terminal detection decides format.
- `--version` returns JSON. `browser text` returns the page text inside JSON; `--raw` is removed.
- Internal protocols are not commands and are out of scope: worker framing (exec, file transfer), and program output that `exec` forwards inside its result.
- **A regular command's stderr is only its one error line.** (Human commands draw their screens where they like, stderr included; this rule does not apply to them.) `LATCH_TRACE` output goes to a file in the state folder, not stderr. Foreground `serve` is a long-running process, not a command result: its logs, trace included, stay on stderr as today.
- **Prompts are not output.** A regular command may ask a person something on stderr only when it is at a terminal: a confirmation (`devices remove` without `--yes`), secret entry (`vault add`, `vault edit --ask`), `vault import`'s preview and picks, `vault link`'s QR code. Its result is still JSON on stdout. Without a terminal it never prompts, and the one-error-line rule holds.
- **The once-a-day "update available" notice leaves regular commands.** Today `update::notice` can print it to stderr after any command when both streams are terminals. Regular commands stop printing it; `status` carries the same fact as a JSON field; the human commands (`setup`, the dashboard) may still show it.

### 2.2 Streams
- `events` (with or without `--follow`), `audit show -f`, `logs [-f]` and `login --events` emit JSON lines. Each line has a `type`; the first line is `{"type":"hello","schema":N}`; consumers ignore unknown fields.
- `logs` emits one JSON line per log record. How records are stored on disk is the implementer's call.

### 2.3 No text mode
- There is no flag or environment variable that turns a regular command's output into text. `status --pretty`'s text card and other per-command text renderings are removed.
- **`--pretty` is a global flag that indents the JSON** (still JSON, same content; chef, 2026-10-22). Default output is compact, one object per line. Streams stay one object per line even with `--pretty`. Errors stay one line on stderr. A person at a terminal reads the JSON; its error `message` is one plain sentence.
- Humans get richer views in the human commands instead (§2.4). The dashboard grows into the human view of status, connectors, rules and the audit over time.

### 2.4 Human commands
- `setup`, interactive `login`, the dashboard (attached `serve`, `dashboard`) and `rules edit` (it opens the person's editor) are for people and are never JSON. Nothing blocks an agent from running them; they just aren't a machine interface. `help`, `--help`, `skill` and bare `latch` (its logo and help) are text by nature.
- Their machine equivalents are regular JSON commands: setup's steps (`plugins enable …`, `status`), `login --events` (JSON lines as today: `code` carrying the link and expiry, `waiting`, `verified`, `expired`, `error`), `serve -d` / `serve --foreground`, and the audit (`audit show [-f]`, `events --follow`) in place of the dashboard.
- Without a terminal, a human command exits 2 with JSON `no_terminal` (the code the dashboard already uses), whose `next` names its machine equivalent. For `setup`, which has no single equivalent, `next` names the first step not yet done (e.g. `latch login --events`, then `latch plugins enable messages`), or `latch status` when everything is done. Only interaction needs a terminal: `login` with an agent credential or an existing login still works non-interactively as today, returning JSON. A `login` with no credential at all and no terminal keeps its own code `no_credential` (exit 2), whose `next` names both ways in: `latch login --token-file PATH, or latch login --events`.
- A human command that fails after it has started talking to the person (e.g. a login by text whose code expired) says so in a plain line on the terminal, not JSON. `setup` running its steps relies on their exit status, not on reading their terminal output.
- latch invoking itself (e.g. `setup` running its steps) parses its children's JSON.

### 2.5 One error shape
- `{"error":{"code":"…","message":"…","next":"…","exit":N, …details}}` on stderr, one line, for every failure: argument parsing, transport, setup, host, connector.
- `message` is one short sentence; usage hints (e.g. exec's "put the command after `--`") go in `next`, not a second paragraph; `next` is at most one command or action (omitted when there is none); `exit` equals the process exit code.
- Argument and usage errors always exit 2, and so does `no_terminal`; consumers branch on `code`, not the exit number. Existing exit classes otherwise stay; every code and its exit is listed in `help --json`.
- A command whose operation succeeded but whose child failed (e.g. `exec` exit 1) returns a result, not an error; its JSON carries the child's status. An `exec` timeout is an error on stderr; any output the program produced before the timeout goes in the error's details.

## 3. Copy guide

Applies to every human-facing string: the human commands' screens and prompts, error `message` and `next`, help one-liners.

Tone (chef, 2026-10-22, from a 16-string sample): **slightly warm and explicit**. Plain sentences in sentence case with a full stop; say the consequence when a person must decide ("Replace it and sign the old one out?"); keep a short hint when it teaches the person something they'll need.

1. Lead with the outcome in one short sentence.
2. Name what was affected (host, connector, path).
3. One next step, as a command when there is one. A short "or" alternative is fine when both are real choices for a person (e.g. "Update this latch, or use another --state-dir").
4. Say refused / unavailable / failed / skipped precisely; never claim success early.
5. No explanation unless something failed or the user must decide. Warnings go where the person acts on them, not on standing screens.
6. No apologies, celebration or exclamation marks.
7. Internal IDs and diagnostics (process ids, protocol names, version strings) go in details. Values the person acts on stay in the sentence: a code to text, host names to pick from, when a host was last seen.
8. Colour and symbols only reinforce words; honour `NO_COLOR`; sanitize terminal control characters.

Secrets disclosure (chef, 2026-10-22): the dashboard's footer line about hiding credential-looking values is removed. The disclosure moves to where people choose how to pass secrets: `latch exec --help` next to `--secret-env`, and the agent instructions `latch skill` prints. `REVIEW.md`'s "best effort, and they say so" note is updated to say where it is said. Redaction itself is unchanged.

Example: `The Messages connector is off. Run: latch plugins enable messages`.

### 3.1 One source for each piece of help (chef, 2026-10-22)

Goal: help stays as verbose and useful as today, for agents above all; what changes is that each fact is written once and every surface that shows it pulls it from that one source. Today the same guidance is hand-copied across `latch skill` (`SKILL.md`), the text appended to `--help` (`AGENT_HELP`, `EXEC_HELP`, `MESSAGES_HELP`, …) and `help --json`, and nothing tests the copies.

- **A command's `--help` stays complete for that command.** An agent that reads only `latch <cmd> --help` still gets everything it needs for that command, including the cross-command rules that apply to it (e.g. `messages --help` still says message content is untrusted; `exec --help` still carries the secrets guidance). Those shared paragraphs are written once and included, not re-typed.
- **Sources** (from `research/research-help-layout-{claude,codex}.md`):
  - clap doc comments: flags, arguments, defaults. `-h` and `--help` stay the same text.
  - `ENTRIES` (behind `help --json`): output, side effects, retry, examples. `--help` shows its examples from there.
  - `Exit` / error codes: exit meanings, shown in `help --json` and in `latch skill`.
  - Shared guidance blocks (untrusted content, secrets, refusals and retries, logins): one source each, included in `latch skill` and in the `--help` of every command they apply to.
  - A command's own grammar and limits (exec's limits, `browser do`'s steps): that command's long help.
- `latch skill` remains the full agent guide (it includes all shared blocks plus browsing, sandbox and sign-in walkthroughs). The root `--help` keeps its command list and points agents to `latch skill` and `help --json`.
- **Drift tests:** every `latch …` line in `SKILL.md` and `--help` parses; every code `SKILL.md` names exists (denial reasons such as `owner_said_no` count); snapshots of `--help` per command and of `skill`.
- **Delete:** only true duplicates — hand-typed copies replaced by the shared blocks — and `help --examples` (a subset of `help --json`). No command loses help content.

## 4. Out of scope

Localization; a GUI itself; changing what commands do (only how they report); where foreground `serve` logs go.

## Chunks

### Global Constraints
- Product stage: internal tool, pre-PMF. Size the review bar to that.
- JSON is the default for every command's result and errors; no terminal detection chooses format (§2.1). The human commands and help text are the only exceptions (§2.4); no text mode for regular commands (§2.3).
- One error shape (§2.5) for every failure path; `exit` in the JSON equals the process exit code.
- No back-compat shims for today's output; delete old text paths rather than keep both.
- The copy guide (§3) governs every human string. Chunks 1a–1d may leave existing message wording as is; Chunks 2–3 own the wording.
- "Every command" means the command list `help --json` reports, so tests cannot drift from the real surface.

### Chunk 1a: One error shape
Implements: §2.5, and the `LATCH_TRACE` rule in §2.1
Interfaces: consumes existing failure codes and exit classes · produces the error shape with `code`/`message`/`next`/`exit`, and `help --json` listing every error code with its exit
Done when: a test triggers one failure per regular command (from `help --json`; human commands excluded) with stderr both a terminal (pty) and piped, and each gives exactly one JSON error line whose `exit` equals the process exit code; bad arguments exit 2 for every command; with `LATCH_TRACE` set or an update available, stderr is still exactly one line and the trace lands in the state folder; `help --json` lists every code with its exit.

### Chunk 1b: JSON results, no text mode
Implements: §2.1 (results, and a regular command writes nothing to stderr when it succeeds: warnings and previews go in the JSON result, installer/download progress goes to `trace.log`, the legacy plaintext-audit notice becomes a result or error field), §2.3
Interfaces: consumes 1a's error shape · produces JSON-only results for every regular command and the global `--pretty`
Done when: tests at the shared result printer show compact one-line JSON, `--pretty` indented with identical content, and the error line's `exit` equal to the process exit; a small integration test runs a few representative commands at a terminal (pty), piped and with `--pretty` and gets identical JSON and empty stderr (chef, 2026-10-23: no per-command inventory — it cost ~4 min on the macOS run); `--pretty` gives indented JSON of the same content for every command; `--version` and `browser text` return JSON and `--raw` is gone; a grep shows the per-command text renderers (including `status --pretty`'s card) are deleted; an `exec` whose child exits 1 returns a result, and an `exec` timeout returns an error carrying the partial output. A successful run of every regular command leaves stderr empty, at a terminal and piped.

### Chunk 1c: Typed streams
Implements: §2.2
Interfaces: consumes 1a's error shape · produces JSON-lines streams with `type` and a `hello` first line
Done when: for `events`, `events --follow`, `audit show -f`, `logs`, `logs -f` and `login --events`, a test reads the stream piped and finds a `hello` line with `schema` first and a `type` on every line; `logs` gives one line per record; `--pretty` does not indent stream lines.

### Chunk 1d: Human commands
Implements: §2.4
Interfaces: consumes 1a's error shape and 1b's JSON commands · produces `no_terminal` for every human command and `setup` driving its steps through their JSON
Done when: `setup`, interactive `login`, attached `serve`, `dashboard` and `rules edit` each exit 2 with `no_terminal` and a `next` naming the machine equivalent when run without a terminal; `login` with an agent credential or an existing login succeeds without a terminal and returns JSON; `setup` still completes in a terminal, with its steps' JSON parsed rather than printed.

### Chunk 2: Copy pass — proposal
Implements: §3, §3.1
Interfaces: consumes every human-facing string after Chunks 1a–1d · produces a before/after table for the chef
Done when: a proposal for the §3.1 help sources (which fact comes from where, which shared blocks each command's `--help` includes) plus a table of every human-facing string (human-command screens and prompts, error messages and `next`, help one-liners, `--help` add-on text, `SKILL.md`), with the current text, the proposed text and where it lives, is delivered to the chef; no code changed.

### Chunk 3: Copy pass — apply
Implements: §3, §3.1
Interfaces: consumes the chef-approved table from Chunk 2
Done when: every approved string is in the code; tests that assert copy are updated; a grep shows none of the replaced strings remain.
