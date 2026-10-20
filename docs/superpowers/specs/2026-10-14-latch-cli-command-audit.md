# latch-cli command audit

> **Historical.** This records the command audit as of 2026-10-14 and is no longer kept up to date. Where it differs, the code, the docs beside it in `latch/` and `2026-10-19-latch-browser-poc-design.md` are current.

Date: 2026-10-14 · Companion to `2026-10-14-latch-cli-poc-design.md` (the approved spec; this file follows its vocabulary and conventions and never overrides it).

What this is: every capability of the existing Electron Latch (its 15 MCP tools and its non-tool features) and every `plow` CLI command, mapped to a `latch` command, with arguments, output, where it runs, and whether it is in the **POC** or **Later**. The mapping is informed by a survey of other CLIs (§1). Sources read: Latch `packages/mcp-server/src/tools.ts` and `packages/device-core/src/providers/` (`plowGog.ts`, `gogSkill.ts`) at `1ea5674e`; this repo's `cli/plow/commands/*.py` at `68c43c6c`.

Vocabulary (spec §2): a **peer** is anything registered on the latch network; on one connection the **client** dials and the **host** answers, checks and does the work. In the POC the only built direction is agent client → latch host, and the POC host build is macOS-only (its sandbox is Seatbelt); Linux hosts follow right after the POC.

---

## 1. Survey of other CLIs

Labels: **[F]** read this session from docs, local `--help` or source; **[K]** well-known behaviour, not re-checked.

| Tool | Grammar | Targeting | Output / errors | Discovery | Long-running | Auth / approvals |
|---|---|---|---|---|---|---|
| rex [F] | `rex run`, `rex send`, `rex capture`; generic `rex api list \| describe <m> \| call <m> <json\|@file\|->` | `-S host -s session -w window -b block`; match order full id → label → position → id suffix ≥4; `REX_*` env defaults | `--json`; indented on a terminal, one line piped. Exit 0 ok, 1 fail, 2 bad command line, 3 target not found/ambiguous (lists matches), 4 server unreachable, 130 interrupted | `api describe --json` returns JSON Schema input/output plus examples, served by the connected server's own version | `rex run --wait`, `rex wait` | unix socket 0600; tailnet listeners |
| gh [F] | noun verb: `gh pr list`, `gh api /repos/…` | `-R owner/repo`, else from the git remote | text by default; `--json fields` (no fields → lists them), `--jq`, `--template` | the `--json` field list | `gh run watch` | `gh auth login/status`; `GH_TOKEN` |
| kubectl [F/K] | verb noun: `kubectl get pods` | `-n`, `--context`; `kubectl cp ./f ns/pod:/tmp/f` | `-o json\|yaml\|name\|jsonpath`; `--dry-run` | `kubectl explain`, `api-resources` (from the server) | `logs -f`, `get -w`, `wait --for` | kubeconfig contexts; `auth can-i` |
| aws / gcloud [K] | service noun verb | `--profile`/`--region`, `--project`; `s3://` and `inst:~/f` path prefixes | `--output`/`--format`, `--query`/`--filter`, `NextToken` | `--generate-cli-skeleton` | waiters, `--async` + `operations wait` | `sts get-caller-identity`; named profiles |
| docker [K] | `docker container ls` (+ legacy `ps`) | `docker cp ctr:/p ./`; `--context` | `--format '{{json .}}'` (NDJSON) | `--help` | `logs -f`, `events`, `run -d` | contexts |
| tailscale [F] | `tailscale file cp <files…> <host>:`, `file get <dir>` | `host:` suffix; `file cp --targets` | `--json` on status | `--targets` | `file get --wait` | a push lands in the receiver's inbox; the receiver must accept |
| flyctl [K] | `fly machine list`, `fly ssh sftp get` | `-a app`, `--machine` | `--json` | `--help` | `fly logs`, `machine wait` | `FLY_API_TOKEN` |
| 1Password `op` [F] | `op item get`, `op read op://vault/item/field`, `op run -- cmd`, `op inject` | `op://` references; `--account` | `--format json` | `--help`, `op whoami` | — | app integration: a biometric prompt per terminal session names the account and the calling app; 10 min idle / 12 h hard; locking revokes all. Service-account tokens are non-interactive and vault-scoped. `op run` hands secrets to a child process without printing them |
| stripe [K] | `stripe customers create`; raw `stripe get /v1/…` | test vs live mode | JSON | `stripe resources` | `stripe listen` | `stripe login` pairing code; `--idempotency-key` |
| gog [F] | `gog gmail search 'newer_than:7d'` | `--account`/`GOG_ACCOUNT` | `--json`; `--wrap-untrusted` marks third-party text | `--help` | — | `--enable-commands` allowlist; globals are last-wins and accepted after the command path (`cli/plow/commands/google.py:132-138`), which is why a gog wrapper must refuse gate-overriding flags before gog starts, not rely on leading flags |
| himalaya [F] | `himalaya envelope list`, `message read 42`, `attachment download 42` | `-b backend`, `-c config` | `--json` | per-command `--help` | — | config accounts |
| Claude Code / Codex [K] | `claude -p --output-format json\|stream-json`, `codex exec --json` | — | NDJSON event streams | — | streaming | allow/deny rules by tool + argument pattern |
| `plow` CLI [F] | `plow calendar today`, `plow slack send --attach` | `--account` | JSON; error `{"error":{path,status_code,detail}}` (`cli/plow/client.py:74-80`); `meta.next_cursor` | argparse help | — | agent-readable token file, no gatekeeper |

Agent-facing guidance: Anthropic's *Writing effective tools for agents* (namespace by service then resource; concise vs detailed responses; paginate and truncate with sensible defaults; actionable error text; human-readable names, ids kept for chaining) and *Code execution with MCP* (load definitions on demand, filter before results reach the context). Practitioner benchmarks (Mario Zechner, *MCP vs CLI*, 2025) find agents do as well shelling out to a good CLI as to an MCP server that re-implements it.

What `latch` takes from this:

1. **Noun verb** (gh, op, tailscale, fly, gog, himalaya). Fits a CLI with many services.
2. **One target flag.** Prefix addressing (`pod:/p`, `host:`) only fits path arguments, so CLIs that use it still need a flag for everything else. `latch` uses the flag alone: `--host <name>`, never a `host:` prefix (spec §6). Direction lives in the verb (`file get` / `file put`).
3. **JSON on stdout, errors on stderr**, with exit codes that tell the agent whether to fix, retry or stop (rex's 3 and 4 are the model).
4. **Discovery served by the target** (rex `api describe`, kubectl `explain`): Later.
5. **Consent on the receiving side** (tailscale's inbox): the host checks and gates every call; the client's claims are never evidence. Hence no `--why`.
6. **Handles for long work** (`--async`, `wait`, `logs -f`): Later, as jobs.
7. **Secrets used where they live** (`op run`): the Google token stays on the host and is never returned to the client.

## 2. Conventions (as in the spec)

- `latch <noun> <verb> [args] [--host <name>]`. Every command takes `--state-dir DIR` (the local instance; default `~/.latch` on every OS). Host resolution: `--host`, then `$LATCH_HOST`, then the one host listed in the directory if there is exactly one; with several, exit 2 asking for `--host` and listing the names. Reachability is decided by dialing, not by the `online` flag. Every command that touches a host takes `--host`; commands that don't (`login`, `status`, `serve`) don't take it.
- Success: one JSON object on stdout. File-moving commands report path, size and audit id, never contents. Failure: `{"error":{"code","message"}}` on stderr. `exec` puts every execution result on stdout, a timed-out one included, and uses the stderr envelope only when no execution result exists.
- Exit codes: 0 ok · 1 operation failed on the host · 2 usage error, or unknown/ambiguous host · 3 host unreachable, nothing sent · 4 denied by the host · 5 outcome unknown (connection lost after sending).
- Identity: the client's own iroh key on every call, including on the same machine. The host resolves the key with Plow; no caller-supplied identity, no profiles.
- Not anywhere: `--why`, `--dry-run`, local-only shortcuts. Later: `--idempotency-key`, jobs, pagination, alternate output formats.
- **Effect** column below (read / write / send / exec) labels what an operation does: read changes nothing; write changes the owner's state with no third party; send makes content leave to a third party (email, Slack, calendar invites); exec runs arbitrary code. In the POC the permission rule is account-wide (spec §4); the label feeds the audit record and the per-command permissions that are Later.

## 3. Existing Latch MCP tools (15)

| # | Today (arguments) | `latch` command | Output | Host | Effect | Stage |
|---|---|---|---|---|---|---|
| 1 | `plow_read_file {path, goal}`; 8 MiB cap; text inline or base64 blob | `latch file get <remote-path> [local-dest] --host H` — with no `local-dest`, the remote basename in the client's current directory; an existing local file is overwritten (like `scp`) | `{path, bytes, audit_id}` of the local copy | any | read | **POC** |
| 2 | `plow_write_file {path, content: string, goal}`; UTF-8 only | `latch file put <local-path> <remote-path> --host H` — binary-safe | `{path, bytes, audit_id}` on the host | any | write | **POC** |
| 3 | `plow_run_command {argv, cwd, read_paths, write_paths, network, apple_events, wait_ms}` | `latch exec --host H [--workdir DIR] [--timeout SECS] -- <argv>…` — an argv list, never re-parsed by a shell (`sh -c '…'` explicitly for a shell); `--workdir` inside a configured area (default: the first); timeout default 60 s, max 600 s, then the process group is killed. Runs in the sandboxed worker: writes only inside the areas, outbound network on, no Apple Events, minimal environment. The per-call `read_paths` / `write_paths` / `network` / `apple_events` declarations are dropped: the host's configured areas bound every call | `{exit_code, signal, timed_out, stdout, stderr, truncated, audit_id}` on stdout; each stream capped at 256 KiB (`truncated` set). latch exits 0 when the command exited, whatever its `exit_code`, or was killed by a signal latch did not send (`signal` set, e.g. `SIGSEGV`). On latch's timeout: `exit_code` null, `signal: "SIGKILL"`, `timed_out: true`, the output so far, latch exits 1. Every execution result, the timed-out one included, is on stdout; the stderr error envelope only when no execution result exists | Seatbelt (macOS) in the POC; Landlock + seccomp on Linux right after (spec Chunk 8, stretch) | exec | **POC** (Later: streaming output, interactive/TTY, Apple Events) |
| 4 | `plow_get_output {handle, since}` | `latch job logs <id> [--since N] [--follow] --host H` — not needed in the POC: `exec` blocks until exit or `--timeout` and returns the capped output | NDJSON output chunks | host holding the job | read | Later |
| 5 | `plow_run_applescript {app, script, args, wait_ms}` | `latch mac applescript --app <App> (--script-file F \| -) -- args… --host H` | `{result, audit_id}` | macOS only | exec | Later (POC `exec` denies Apple Events, so `osascript` against another app is refused) |
| 6 | `plow_list_skills {}` | `latch skill ls --host H` | `{items:[{name, description}]}` | any (built-in content is macOS-only) | read | Later |
| 7 | `plow_read_skill {name}` | `latch skill show <name> --host H` | the skill's markdown | any | read | Later |
| 8 | `plow_history {limit}` | `latch audit ls [--limit N] [--since T] --host H`, `latch audit show <id> --host H` | the caller's own audit records | any | read | Later (POC: audit file on the host) |
| 9 | `plow_browser_open {origins, headed}` | `latch browser open --origin <pattern>… [--headed] --host H` | `{session, audit_id}` | any | exec | Later |
| 10 | `plow_browser_request {session, origins, credential_items}` | `latch browser grant <session> [--origin …]… [--credential <vault-item>]… --host H` | `{session, origins, credentials}` | any | exec | Later |
| 11 | `plow_browser {session, action, …}` — 18 actions | one verb per action, all `--host H`: `goto <s> <url>`, `click <s> <selector> [--frame N] [--timeout MS]`, `click-at <s> <x> <y>`, `fill <s> <selector> <value>`, `fill-secret <s> <selector> --item I --field F\|totp`, `scroll <s> down\|up\|top\|bottom`, `wait <s> <seconds>`, `back <s>`, `eval <s> <expr>`, `use-page <s> <i>`, `pages <s>`, `screenshot <s> --out ./shot.jpg`, `text <s> [--max-chars N]`, `url <s>`, `title <s>`, `links <s>`, `forms <s>`, `tables <s>` | JSON; a screenshot is written to a client-side file and reported by path, never as image bytes | any | read: pages, screenshot, text, url, title, links, forms, tables · exec: the rest | Later |
| 12 | `plow_vault {action: list\|describe, item, query}` | `latch vault ls [--query Q] --host H`, `latch vault describe <item> --host H` | items and field labels, never values | any | read | Later |
| 13 | `plow_browser_close {session}` | `latch browser close <session> --host H` | `{closed: true}` | any | read | Later |
| 14 | `plow_device_status {}` | `latch status` (hosts listed, `online`) in the POC; Later `latch host describe <name>` (capabilities, sandbox layers, macOS privacy grants) | peer records | any | read | **POC** (`status`) / Later (`host describe`) |
| 15 | `plow_get_result {handle}` | folded into `latch job get <id>` / `latch job wait <id> --host H` | job record | host holding the job | read | Later |

## 4. Existing Latch non-tool capabilities

| Today | `latch` command | Host | Effect | Stage |
|---|---|---|---|---|
| gog plugin (`plow-gog`: Gmail and Calendar through tokens minted for every connected account) | `latch google --host H [--account EMAIL[,EMAIL…]] [--confirm-conflict] -- <gog args>…`, plus `latch google accounts`: gog's own command grammar, pinned gog (sha256-checked) in the sandboxed worker, fixed safety flags, exactly the spec §6 Google list enabled (§5 below). Keeps `plow-gog`'s extras: fan-out reads merged across accounts, `accounts`, `--send-updates all` on calendar writes, the conflict check on timed creates (`--confirm-conflict` after the owner agrees). Files gog reads and files gog writes are both client-side paths; downloads land on the client as ordinary local files. Refused before gog starts (exit 4, `not_allowed`): commands outside the list, safety-flag overrides, base-URL/proxy flags, `--inline` | host holding the owner's tokens | per gog command (§5) | **POC** |
| messages plugin (`search`, `thread`, `chats`, `unreplied`) | `latch imessage search <q> \| thread <id> \| chats \| unreplied --host H` | macOS only (Full Disk Access) | read | Later |
| wiki plugin (`wiki` CLI with a fixed `WIKI_PATH`) | not its own noun: `latch exec --host H -- wiki …` once the binary and its wiki folder sit in a configured area (the POC worker's minimal environment does not set `WIKI_PATH`, so the call sets it, e.g. `sh -c 'WIKI_PATH=… wiki …'`), or a later generic `latch plugin run <name> -- args` | any | exec | reachable via POC `exec`; plugin wiring Later |
| iMessage, WhatsApp, Contacts built-in skills | stay skills (`latch skill show`); they drive `exec` / `applescript` (the ones needing Apple Events or Full Disk Access do not work under POC `exec`) | macOS only | — | Later |
| Payment gate (`/v1/payment-approvals/consume`) | inside the host's gatekeeper for browser purchases; no client verb | any | send | Later |
| Credential import (CSV, 1pux, Apple Passwords) | **cut**: an owner-only feature of the Electron app | — | — | cut |
| Approval modes (approve / adversarial / ask / deny) and rules | host config; `latch rules ls \| revoke <id> \| revoke --all` for the owner only | any | — | Later |
| Device identity (`identity.json`, Ed25519) | `latch login` creates the key; `latch status` shows it | any | — | **POC** |
| Activation login, relay registration | `latch login --name <name> --token-file F` (host); `latch login` (agent); `latch serve` | any | — | **POC** |
| keep-awake, updater, privacy-grant flow, tray, analytics | stay in the Electron app | — | — | cut from the CLI |

## 5. `plow` CLI commands

| Today | `latch` command (all take `--host H`) | Output | Effect | Stage |
|---|---|---|---|---|
| `calendar list` | `latch google --host H [--account EMAIL[,EMAIL…]] -- calendar calendars` (fan-out) | `{items, accounts, audit_id}`, items tagged by account | read | **POC** |
| `calendar today [--calendar C]` | `latch google --host H [--account EMAIL[,EMAIL…]] -- calendar events --from <today start> --to <today end> [--calendars IDS]` (fan-out) | `{items, accounts, audit_id}`, items tagged by account; merged event lists trimmed to scheduling fields unless `--select` | read | **POC** |
| `calendar search --query --max --start --end` | `latch google --host H [--account EMAIL] -- calendar search …` (gog's own query, window and `--max` flags; one account) | `{account, result, files, audit_id}` | read | **POC** |
| — (`plow-gog` only) | `latch google --host H [--account EMAIL] -- calendar event …` (one event) | `{account, result, files, audit_id}` | read | **POC** |
| `calendar freebusy --start --end [--calendar]…` | `latch google --host H [--account EMAIL[,EMAIL…]] -- calendar freebusy --cal IDS --from T --to T` (fan-out) | `{items, accounts, audit_id}`, items tagged by account | read | **POC** |
| — (`plow-gog` only) | `latch google --host H [--account EMAIL[,EMAIL…]] -- calendar conflicts --from T --to T` (fan-out) | `{items, accounts, audit_id}`, items tagged by account | read | **POC** |
| `calendar create --summary --start --end [--description --location --attendee… --calendar --timezone --add-meet --recurrence]` | `latch google --host H [--account EMAIL] -- calendar create <calendarId> --summary S --from T --to T …` [`--confirm-conflict`] — `--send-updates all` added unless given; a timed create is checked against every account's busy time first (exit 4 `calendar_conflict`, busy spans only) | `{account, result, files, audit_id}` | send with attendees, else write | **POC** |
| `calendar update --event-id --calendar …` | `latch google --host H [--account EMAIL] -- calendar update …` — `--send-updates all` added unless given | `{account, result, files, audit_id}` | send if attendees are notified, else write | **POC** |
| — (`plow-gog` only) | `latch google --host H [--account EMAIL] -- calendar move …` — `--send-updates all` added unless given | `{account, result, files, audit_id}` | write (send if attendees are notified) | **POC** |
| `calendar delete --event-id --calendar` | `latch google --host H [--account EMAIL] -- calendar delete …` — `--send-updates all` added unless given | `{account, result, files, audit_id}` | write (send if it cancels for attendees) | **POC** |
| — (`plow-gog` only) | `latch google --host H [--account EMAIL] -- calendar respond …` (accept/decline an invite) | `{account, result, files, audit_id}` | send | **POC** |
| `gmail search -q --after --before --from --max --cursor` | `latch google --host H [--account EMAIL[,EMAIL…]] -- gmail search '<gmail query>' [--max N]` (fan-out; dates and sender go in the Gmail query); `gmail messages search` for message-level rows (one account) | `{items, accounts, audit_id}`, items tagged by account, newest first | read | **POC** |
| `gmail status` | `latch google accounts --host H` (answered from the token mint, no Google call) | connected accounts and default | read | **POC** |
| `gmail disconnect`, `gmail set-default` | **cut**: owner account bookkeeping, done in Plow or the Electron app | — | — | cut |
| `google gog <argv…>` (allowlist; `--attach`, `*-file`, `--out*` refused) | `latch google --host H [--account EMAIL] -- <gog args>…` — the same gog grammar, run on the host, limited to the spec §6 Google list. Gmail reads: `search`, `messages search`, `get`, `thread get`, `labels list`, `drafts list`, `drafts get`, `attachment`. Gmail writes: `send`, `reply`, `reply-all`, `forward`, `drafts create\|update\|reply\|reply-all\|forward\|send\|delete`, `archive`, `mark-read`, `unread`, `messages modify`. File flags change: `--attach` and `*-file` name client-side files streamed to the host; `--out*` names a client-side destination — gog writes into a per-call worker folder and the host streams each file back, where it lands as a local file. `--inline` (content in the JSON) is refused | `{account, result, files, audit_id}`; `files` lists each downloaded file as `{path, size, sha256}` (client path), never contents. Errors: exit 1 `google_failed` (gog's exit code + fixed reason), exit 1 `output_too_large` above 1 MiB, exit 2 `account_required` / `unknown_account` | send: send, reply, reply-all, forward, drafts send · write: drafts edits, unread, modify, archive, mark-read · read: the rest (`attachment` writes only on the client) | **POC** |
| `email-line send`, `email-line threads` | **out of scope**: the agent's own Plow line, reached with its chat credential, not an owner connector | — | — | cut |
| `profile get/set` | **cut**: Plow account profile | — | — | cut |
| `skill print <path>` | **cut**: prints a local file; host skills are `latch skill show` | — | — | cut |
| `slack channels --limit` | `latch slack channels [--limit --cursor]` | channels | read | Later |
| `slack messages --channel --limit --cursor` | `latch slack history <channel> [--limit --cursor]` | messages | read | Later |
| `slack send --channel --text --thread-ts [--attach]` | `latch slack send <channel> [--text T] [--thread TS] [--attach F]…` | message + audit id | send | Later |
| `slack search <q> --limit` | `latch slack search <q> [--limit]` | messages | read | Later |
| `slack users --limit` | `latch slack users [--limit --cursor]` | users | read | Later |
| `slack open-dm --user` | `latch slack dm <user>` | `{channel}` | write | Later |
| `slack update --channel --ts --text` | `latch slack edit <channel> <ts> --text T` | message | send | Later |
| `slack upload --channel --path --filename --title --thread-ts` | folded into `latch slack send --attach` (`--name`, `--title`) | file + audit id | send | Later |
| `slack status`, `disconnect`, `set-default` | status → `latch slack status`; the others **cut** | — | — | Later / cut |

Excluded from `latch google` (spec §6), although today's `plow-gog` lets them through: Gmail `watch` and `settings watch` (post message contents to any URL); Gmail `track` (tracking pixels via an outside service); Gmail `settings` (filters, forwarding, auto-forward, delegates, send-as, vacation) and `autoreply` (standing rules on future mail); Gmail `trash`, `batch` and thread-wide label edits (irreversible or bulk); Gmail `import`; Calendar `subscribe`, `unsubscribe`, `create-calendar`, `delete-calendar` and `acl`. Every other gog command and Google service is Later, each after a review of its file and network flags; Drive, Docs, Sheets and Contacts also need new OAuth scopes.

Overlapping Hermes plugin tool: `plow_send_email {action, to, subject, body}` (no attachments) is covered by `latch google -- gmail send … --attach F`. Chat `MEDIA:/path` attachments stay in the chat platform; that path is already by-file and never through the model.

## 6. POC command set

The seven commands of spec §6, and the row above each comes from:

| Command | Replaces |
|---|---|
| `latch login` | Latch activation login and relay registration |
| `latch status` | `plow_device_status` (reachable hosts) |
| `latch file get <remote-path> [local-dest] --host H` | `plow_read_file` |
| `latch file put <local-path> <remote-path> --host H` | `plow_write_file` (now binary-safe) |
| `latch google --host H [--account EMAIL[,EMAIL…]] [--confirm-conflict] -- <gog args>…`, `latch google accounts` | the `plow-gog` plugin, every `plow calendar` / `plow gmail` read and write, `plow google gog`, `plow_send_email` |
| `latch exec --host H [--workdir DIR] [--timeout SECS] -- <argv>…` | `plow_run_command` (+ `plow_get_output` for runs that fit the timeout) |
| `latch serve` | the Electron app's device side (host role only; runs in the foreground) |

## 7. Gaps the mapping closes or exposes

1. **Binary write to the host** — impossible today (`content` is a string); closed by `file put`. POC.
2. **Email with an attachment from a cloud agent** — impossible today (`--attach` refused, no attachment field); closed by `latch google -- gmail send … --attach F`, with the token kept on the host. POC.
3. **Files over about 6 MiB** — today base64 inside an 8 MiB relay body; `file get` / `put` stream over iroh. POC.
4. **Choosing the machine** — today one account alias picks the least-idle device; `--host` makes it explicit. POC.
5. **Unknown outcome** — today indistinguishable from failure; exit 5. POC.
6. **Inbound attachments on the Plow email line** — dropped server-side; not fixable in `latch`, needs a server change. For the owner's own Gmail, `latch google -- gmail attachment <msg> <att> --out ./file` lands the attachment as a local file on the client, with only path, size and sha256 on stdout (POC).
7. **Remote listing** — in the POC through `latch exec --host H -- ls -la DIR` (inside the areas); dedicated `latch file ls` / `file stat` with structured output Later.
8. **What may I do here?** — no equivalent today; `latch host describe` and `api list/describe`. Later.

## 8. Cut list

`plow_get_result` (folded into jobs), `plow_browser_request` as a separate tool (→ `browser grant`), `gmail`/`slack` `disconnect` and `set-default`, `profile`, `skill print`, `email-line`, credential import, `wiki` as its own noun, the 8 MiB read cap and inline file contents, the typed `latch gmail` / `latch calendar` verbs (replaced by `latch google`), the gog commands excluded from `latch google` (§5) and gog's `--inline`, and the `--why`, `--dry-run` and `host:`-prefix forms from earlier drafts.
