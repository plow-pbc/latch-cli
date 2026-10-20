# latch-cli proof of concept — design

> **Historical.** This records the proof-of-concept design as of 2026-10-14 and is no longer kept up to date. Where it differs, the code, the docs beside it in `latch/` and `2026-10-19-latch-browser-poc-design.md` are current.

Date: 2026-10-14 · Owner: head chef · Stage: **proof of concept on the path to the real product**, run on owned test accounts and test files. It is not throwaway: the code should be clean and simple enough that later work builds directly on it. Reviewers hold code quality and structure to that bar, but do not demand the production hardening the "Later" section defers (scoped credentials, revocation, self-hosted relays and so on).

## 1. Goal

A new command-line `latch`, written from scratch in Rust in the plow monorepo, that an agent shells out to when it needs something from another machine. The proof of concept shows, end to end:

- an agent's `latch` client reaches a latch host running `latch serve` over a **direct, encrypted iroh connection** (relay fallback), with Plow acting only as a directory;
- the latch host knows **which agent** is calling from the connection itself, checks it with Plow, gates the call, runs it in a sandbox, and audits it;
- files move as files (`latch file get` / `put`), never through the model's context;
- Google parity with today's Latch: the Gmail and Calendar commands agents use through Latch's `plow-gog` today (the §6 list) work through `latch google`, including sending attachments and downloading them as local files;
- a command runs on the latch host (`latch exec`) inside the same sandbox, with its output returned to the agent;
- an account with **several hosts** routes each call to the right one.

The existing Electron Latch app, its relay (`/v1/relay/*`) and its MCP tools are untouched. Its code is reference material only; nothing is imported.

## 2. Vocabulary

- **Peer**: anything registered on the latch network — an owner's computer, an agent, later other machines. Every peer has an ed25519 key (iroh's `EndpointId`), a name unique within its account across all kinds, a kind (`device` or `agent`), and optionally a relay URL where it accepts connections.
- **Client / host**: the role on one connection. The client dials; the host answers, checks and does the work. Roles belong to connections, not machines: any peer may be a host, a client, or both.
- **Plow directory**: the `/v1/latch/peers` routes (§5). Plow never carries file or request bytes.

In this POC the only built direction is *agent client → latch host*. The POC host build is macOS-only because its worker sandbox is Seatbelt (§7, §9); Linux hosts follow right after the POC (§10). Nothing in the API, data model or protocol may assume that agents are only clients or that hosts only run macOS.

## 3. Architecture

```
agent machine (cloud container, or anywhere)          latch hosts (one or more)
┌───────────────────────────────┐                     ┌────────────────────────────────────┐
│ agent ──shell──▶ latch (client)│◀══ iroh: QUIC, ═══▶│ latch serve (host)                  │
│        holds the agent's key   │   direct UDP or    │  verify caller ▸ gate ▸ audit       │
└───────────────┬───────────────┘   relay fallback   │  ▸ sandboxed worker (files)         │
                │                                     │  ▸ gog in worker (token stays here) │
                │  HTTPS: register / list hosts       └──────────────┬─────────────────────┘
                ▼                                                    │ HTTPS: register + heartbeat,
         Plow API  /v1/latch/peers  ◀────────────────────────────────┘ "who is this key?", Google token
```

One Rust binary. `latch serve` is the host role; every other subcommand is a client. The client holds the agent's own iroh key and uses the agent's existing Plow credential (which the agent already holds today) only to log in and to list hosts. No host credential, owner token or Google secret ever exists on the agent side, so the agent's machine needs no separate user for latch. The host holds its key, its Plow credential, its audit log and its rules, and keeps all of them out of the worker's reach.

Every call is an iroh connection authenticated by the caller's key, including when client and host run on the same machine. There is no local fast path, no peer-uid identity, and no "if local / if remote" branch anywhere.

## 4. Identity

- Each peer generates its ed25519 secret key locally on first use and never sends it anywhere. Client key: in the agent's own latch state directory (it is the agent's identity; the agent may read it). Host key: in the host's state directory, readable by the host's user only and excluded from the worker sandbox.
- A peer registers its public key with Plow using an existing Plow credential (§5.1). Plow derives the kind from the credential, never from the request. An owner's machine with no credential gets one by texting a code to Plow (`latch login`, §6), which yields an owner login.
- A host learns the caller's key from the iroh connection (`remote_id`), never from anything the caller sends, and resolves it with `GET /v1/latch/peers/{key}` on connection **and before every operation**. Any lookup failure — unknown key, other account, revoked credential, Plow unreachable — denies. No positive caching.
- POC permission rule: a live peer on the same account as the host may call it. There are no per-command or per-agent permissions yet; the worker's pinned areas (§7) bound what any call can touch.

## 5. Plow API

All routes are new, additive and isolated: a new router under `/v1/latch`, a new `latch_peers` table. No change to `/v1/relay/*`, `relay_devices`, `agents`, `/agents/me`, the relay WebSocket or MCP. The API ships before the CLI; the CLI requires these routes and has no fallback.

**Owner login.** The latch routes accept two owner credentials: the owner portal login (`is_portal_login`), and the `*:*` token that device activation returns (`POST /v1/auth/activate` and `/activate/redeem`, the flow the old Latch Mac app used). Accepting the activation token is a check inside the latch router only; `is_portal_login` and the agent web-page rule in `auth.py` are unchanged. This is a deliberate POC choice: the activation token is broad (`*:*` reaches every scope of the owner's account), and a narrower token for latch logins is Later (§10). A credential is first resolved as an agent credential; only one that is not an agent credential can be an owner login.

### 5.1 `POST /v1/latch/peers` — register me (idempotent; doubles as heartbeat)

Auth and kind, decided once from the credential:
- a current live agent credential (resolved the same way `GET /agents/me` resolves its agent) → `kind: agent`; name from the request, defaulting to the agent record's name;
- an owner login (portal login or activation token, above) → `kind: device`, name from the request;
- anything else → 403.

Request: `{"key": "<64 hex>", "name": "<name>", "relay_url": "<https url>"}`. Devices must send `name`; agents may. A peer that accepts connections sends `relay_url` (in this POC: hosts). Agents send no `relay_url` in the POC, but the schema allows `relay_url` for any kind so an agent can later become a host.

Behaviour: first call creates the peer. Repeating with the same key from the same account and kind is not an error: it refreshes `relay_url` and `last_seen_at` and returns the peer. A repeat that omits `relay_url` keeps the stored value; one that sends it, `null` included, replaces it. A key already bound to a different account, kind or agent → 409. A name already used in the account by a different key, of any kind → 409, except when the request sets `"take_over": true`, the peer is a `device`, and it was registered by the caller's own session: then the peer takes the new key in place (same name, `relay_url` and `last_seen_at` refreshed) and the call returns 200, since the caller already holds the login that owns it. Only `latch login` sends `take_over`; heartbeats never do, so a machine whose key was taken over gets 409 on its next heartbeat and never reclaims the name. Exactly one key holds a name at a time (names are one account-wide namespace, so `--host <name>` is never ambiguous). One key per agent. A peer whose registering credential is no longer live gives up its key, name and agent to a registration from the same account, which replaces it; another account still gets 409. Response 200: the peer record (shape below).

A running host re-sends this every 10 minutes, with ±20% random jitter so hosts never synchronise; that is the heartbeat. The server writes only when something changed: a new `relay_url`, or a stored `last_seen_at` older than 5 minutes. Otherwise it returns the stored peer without a database write. On any error or 429 the host backs off exponentially (with jitter, capped at 1 hour, honouring `Retry-After`); it never retries in a tight loop.

### 5.2 `GET /v1/latch/peers` — list my account's peers

Auth: agent credential or owner login. Returns every peer in the caller's account whose registering credential is still live: devices and agents, with or without a `relay_url`, online or not. Clients filter it (§6): hosts are the peers with a `relay_url`, machines are the `device` peers.

```json
{"data": [{"key": "…", "kind": "device", "name": "mac-studio", "relay_url": "https://…",
           "last_seen_at": "2026-10-15T01:30:12Z", "online": true}]}
```

`online` is `last_seen_at` within the last 25 minutes (two missed heartbeats plus jitter). It means "checked in recently", not "reachable"; the dial is the real test.

### 5.3 `GET /v1/latch/peers/{key}` — who is this?

Auth: owner login (the host's credential). Returns the peer with that key if it is in the caller's account and its registering credential is still live — any kind:

```json
{"key": "…", "kind": "agent", "name": "research-agent", "account_uid": "…", "agent_uid": "…", "permissions": {"call": true}}
```

Anything else → 404 (same response for unknown, other account, revoked). 422 for a malformed key.

### 5.4 Data model

`latch_peers`: key (unique), user (account), kind (`device` | `agent`), agent (nullable, unique, set iff kind is agent), session that registered it, name, relay_url (nullable), last_seen_at, timestamps. Unique (account, name) across all kinds. One autogenerated migration. No revoked column: a peer stops working when the credential that registered it is revoked or idle-expired.

### 5.5 Google tokens

The host gets short-lived Google tokens for all of the owner's connected Google accounts from the existing `POST /v1/connectors/gmail/access-token`, requesting all accounts rather than the single-account default, with its owner login (both kinds carry the `gmail` scopes it needs). No change to that route. Plow's Google grant today covers Gmail and Calendar only; other Google services need new OAuth scopes and are Later.

### 5.6 `DELETE /v1/latch/peers/{key}` — remove a machine

Auth: owner login; an agent credential or anything else → 403. Device peers only. In one transaction it deletes the peer and revokes the Plow session that registered it, so that machine, still running or stolen, can neither heartbeat nor register again with its old token. Revoking signs that login out everywhere it is used. 404 when the key is not a peer in the caller's account. 409, code `not_a_device`, when the peer is an agent (agent peers are never removed here). 409, code `own_machine`, when the peer was registered by the caller's own session, since removing it would sign the caller out. Response 200: the removed peer.

### 5.7 `POST /v1/latch/peers/{key}/offline` — going offline

Auth: the credential that registered the peer; any other caller gets 404, so no one learns another peer exists. Clears the peer's `last_seen_at`, so §5.2 reports it `online: false` at once; the peer, its name and its `relay_url` stay. Idempotent: a peer already offline returns 200. Response 200: the peer record. The next §5.1 call from that peer always writes (a cleared `last_seen_at` counts as older than 5 minutes), so a restarted host is online on its first heartbeat. A host that dies without calling it (crash, power loss, lost network) still shows online until the 25-minute window passes; that is accepted.

## 6. Client commands

Nine commands: the POC subset of the full command audit in `2026-10-14-latch-cli-command-audit.md` (beside this spec), which maps every existing Latch tool and `plow` CLI command, informed by a survey of rex and other CLIs, to a `latch` command or to Later. Every command takes `--state-dir DIR`, which selects the local latch instance (its key, Plow credential and config, and on a host its audit log); the default is `~/.latch` on every OS. A local dev build may bake in a different default state directory and Plow API URL at compile time (`LATCH_BUILD_STATE_DIR`, `LATCH_BUILD_API_URL`); flags still override them, `latch --version` names them, and release builds set neither. Every command that touches a host takes `--host <name>`; with no flag it uses `$LATCH_HOST`, else the one host in the directory (§5.2: a peer with a `relay_url`) if there is exactly one; with several it exits 2, asking for `--host` and listing the names. Whether a host is reachable is decided by dialing it, never by the `online` flag. No command parses a `host:` prefix.

| Command | Does |
|---|---|
| `latch login [--name N] [--token-file F]` | Creates this peer's key if missing and registers it (§5.1). Credential, in order: `--token-file` when given (saved as this machine's owner token); else the owner token already saved in the state directory (by an earlier `--token-file` or text-to-login); else an agent credential, found where latch looks for it today; else, on a terminal only, text-to-login (below). With no terminal and no credential it exits 2 telling the user to pass `--token-file`; it never waits on a text nobody sees. A device login with no `--name` asks for one on a terminal. Re-running `login` on a logged-in machine changes nothing. |
| `latch devices` | Lists the account's machines: the `device` peers from §5.2, online or offline, with `last_seen_at`, as JSON. Agents are not listed. |
| `latch devices remove <name> [--yes]` | Removes a machine: finds the `device` peer with that name in §5.2's list (none → exit 1, `not_found`) and deletes it by key (§5.6), which removes its directory entry and signs out the Plow login that registered it. On a terminal it asks first, saying this signs that machine out and that anything else sharing that login (for example the Plow Mac app) is signed out too; with no terminal it requires `--yes` (else exit 2). It refuses the caller's own machine (exit 2, code `own_machine`). Owner logins only; an agent gets exit 4. |
| `latch status [--pretty]` | Prints this peer's identity and the hosts it can reach (the peers in §5.2 with a `relay_url`), as JSON. `--pretty` prints a card for a person instead: identity, the key as OpenSSH-style key art, and the hosts with theirs (Chunk 9); colored only on a terminal without `NO_COLOR`. |
| `latch file get <remote-path> [local-dest] --host H` | Copies a file from the host to the client. Binary-safe, streamed, any size the transport carries. With no `local-dest` it writes the remote file's basename in the client's current directory; an existing local file is overwritten (like `scp`). |
| `latch file put <local-path> <remote-path> --host H` | Copies a file from the client to the host. Binary-safe. |
| `latch google --host H [--account EMAIL[,EMAIL…]] [--confirm-conflict] -- <gog args>…` | Runs one command from the Google list below with the Google CLI `gog` on host H, with the owner's Google token supplied by the host, and returns gog's JSON. The fan-out reads run on every connected account (or on the accounts named) and come back merged; every other command runs on one account and needs `--account` when more than one is connected. `latch google accounts` lists the connected accounts. Calendar writes get `--send-updates all` unless the agent chose a mode, except `respond`, which uses gog's default because the pinned gog has no `--send-updates` for it; a timed calendar create is first checked against the busy time of every connected account. File arguments gog reads (attachments, body files) are client-side paths whose bytes are streamed to the host with the request. File arguments gog writes (an attachment download, a thread's attachments) are client-side paths too: gog writes into a per-call folder in the worker, and the host streams each file back to the client, where it lands at that path as an ordinary local file. `--inline`, which puts attachment content into gog's JSON, is refused. |
| `latch exec --host H [--workdir DIR] [--timeout SECS] -- <argv>…` | Runs one program on host H inside the sandboxed worker (§7) and returns its result. Arguments are passed as an argv list, never re-parsed by a shell; to use a shell the agent runs `sh -c '…'` explicitly. `--workdir` must be inside a configured area (default: the first area). Default timeout 60 s, maximum 600 s; on timeout the process group is killed. |
| `latch serve [--no-dashboard]` | Runs the host (§7) in the foreground, as launchd and other service managers expect. There are no install, start or stop commands in the POC. On a terminal it shows a live dashboard and logs to `serve.log` in the state directory (Chunk 9); `--no-dashboard`, `NO_COLOR` or a non-terminal stdout keep the plain log on stderr. |
| `latch` (no arguments) | On a terminal, shows the Plow logo with its animation (`NO_COLOR`: the finished logo, uncolored); anywhere else, the usual usage error (exit 2). |

**Text-to-login.** `latch login` calls `POST /v1/auth/activate` (with the machine's name), prints `Text "Plow Activate: <code>" to <send_to>` once, notes that whoever texts the code first gets the login, and on macOS also opens Messages prefilled (`sms:<send_to>?&body=…`). It then polls `POST /v1/auth/activate/redeem` about every 3 s until `verified` or until the code expires (410, after 30 minutes), and stops on 404. It keeps the token from the first `verified` answer, since later answers carry `null`, registers at once with it, and saves it in the state directory like a `--token-file` token. The code is never written to any log. The account is the one belonging to the phone that texted.

**Name already taken.** When `login` gets 409 because another of the account's machines has the name: on a terminal it asks `A machine named <name> already exists (last seen <time>). Replace it? This signs that machine out.`; yes finds that machine's key in §5.2's list, removes it (§5.6) and registers, no exits 2. With no terminal it exits 2 (code `conflict`) with an error naming `latch devices remove <name>`. There is no `--replace` flag. A name held by an agent is never offered for replacement: it exits 2 naming the conflict. The prompt's last-seen time comes from §5.2.

**Google list.** `latch google` runs exactly these gog commands (gog's own aliases for them included), which are the ones agents use today. `--help` on any of them returns gog's usage.

- Gmail reads: `search`, `messages search`, `get`, `thread get`, `labels list`, `drafts list`, `drafts get`, `attachment` (downloads to a client path).
- Gmail writes: `send`, `reply`, `reply-all`, `forward`, `drafts create`, `drafts update`, `drafts reply`, `drafts reply-all`, `drafts forward`, `drafts send`, `drafts delete`, `archive`, `mark-read`, `unread`, `messages modify`.
- Calendar reads: `calendars`, `events`, `event`, `search`, `freebusy`, `conflicts`.
- Calendar writes: `create`, `update`, `move`, `delete`, `respond`.
- Fan-out reads (merged across accounts): Gmail `search`, Calendar `calendars`, `events`, `freebusy` and `conflicts`.

Deliberately excluded, although today's `plow-gog` lets them through:

- Gmail `watch` and `settings watch`: they post message contents to any URL and run a listener, a standing exfiltration channel rather than an action.
- Gmail `track`: deploys a tracking service through an outside tool and embeds tracking pixels in mail.
- Gmail `settings` (filters, forwarding, auto-forward, delegates, send-as, vacation) and `autoreply`: standing rules that act on future mail with no later call to gate.
- Gmail `trash`, `batch` and thread-wide label edits: irreversible or bulk changes beyond the message the agent named.
- Gmail `import`: inserts arbitrary messages into the mailbox.
- Calendar `subscribe`, `unsubscribe`, `create-calendar`, `delete-calendar` and `acl`: they change which calendars exist or who can see them.
- Every other gog command and Google service: not used by agents today. Adding one is a change to this list plus a review of its file and network flags.

Output: on success, one JSON object on stdout; file commands report path, size and the audit id, never contents. `exec` returns `{"exit_code", "signal", "timed_out", "stdout", "stderr", "truncated", "audit_id"}`. `exec` and `google` are the two commands whose responses carry data from the host: `exec` returns whatever the command printed, by design, because that is what running a command means, and `google` returns what gog read from Google, because reading mail and calendars is the operation. Each `exec` stream is capped at 256 KiB in the response (the rest is dropped and `truncated` is set), so a runaway command cannot flood the agent's context. Results: a command that exited normally gives `exit_code` set, `signal` null, `timed_out` false, and latch exits 0 whatever the command's own code; the agent reads `exit_code`. A command killed by a signal it did not get from latch gives `exit_code` null, `signal` set (e.g. `"SIGSEGV"`), and latch exits 0. A command killed by latch's timeout gives `exit_code` null, `signal: "SIGKILL"`, `timed_out: true`, the output captured so far, and latch exits 1. Every `exec` execution result, a timed-out one included, goes on stdout. A failure with no execution result (and every failure of the other commands) gives `{"error": {"code": "...", "message": "..."}}` on stderr.

`google` results:

- **One account.** `{"account", "result", "files", "audit_id"}`. `result` is gog's JSON. `files` lists each file gog wrote, as `{"path", "size", "sha256"}` with the client-side path, and never its contents. gog's "no results" is a success with an empty `result`.
- **Fan-out read.** `{"items", "accounts", "audit_id"}`. `items` is the merged result, each item tagged with its `account`; mail is newest first and calendar soonest first. Merged calendar event lists are cut down to the fields scheduling needs (summary, day, local start and end, other attendees, free or declined markers, id, account) unless the agent passes its own `--select`. `accounts` has one entry per account asked: `status` is `ok`, `empty` or `failed`, with a fixed `reason` when failed. Latch exits 0 when at least one account answered, even if others failed, and 1 when none did. The agent reports failed accounts rather than reading them as an absence.
- **gog fails** on a one-account command: exit 1, code `google_failed`, with gog's exit code and a fixed reason from gog's published exit codes (needs re-auth, permission denied, rate limited, invalid request, not found). gog's own error text is not passed through.
- **Refused before gog starts:** a command not on the Google list, a flag that would override the safety flags or redirect gog's network, or `--inline` gives exit 4, code `not_allowed`. A missing `--account` where several accounts are connected, or an account that is not connected, gives exit 2, code `account_required` or `unknown_account`, with the connected accounts listed.
- **Calendar conflict.** The check covers, on each connected account, the calendars that account owns or can write; read-only and subscribed calendars (holidays, calendars others share read-only) are not checked. A timed create that overlaps busy time on any connected account, or whose check could not cover every account, gives exit 4, code `calendar_conflict`, with the busy spans per account (times only, no event titles) and the accounts that could not be checked. Re-sending the same command is refused again; a new command with `--confirm-conflict` books it anyway, and is for after the owner has agreed.
- **Too much output.** gog JSON above 1 MiB fails the call with exit 1, code `output_too_large`, rather than being cut, because a cut read looks complete. The agent narrows the request (`--max`, `--select`, a tighter query).

**plow-gog parity (chef, 2026-10-18).** `latch google` stays an opinionated layer and copies the opinions of today's plow-gog planner (the old Latch app's `packages/device-core/src/providers/plowGog.ts`, the same code Plow's `/v1/connectors/google/run` runs), changing the rules above where they differ:

- `--account` (a comma list allowed) and `--confirm-conflict` are also accepted anywhere inside the gog args and lifted out, as plow-gog does.
- Fan-out reads run across accounts in parallel, at most 4 at once. The `{items, accounts}` envelope above stays.
- Calendar event lists default to `--max 100` and the owner's time zone (the host's local zone), as plow-gog does.
- Merged calendar events take plow-gog's compact shape in the owner's zone: `summary`, `startDayOfWeek`, `startLocal`, `endLocal`, `allDay`, `attendees`, `transparency`, `declined`, `id`, `account` (gog emits the day-of-week and local fields itself). A merged calendar answer over plow-gog's budget (40,000 characters) is cut at the last whole event and carries `truncated: {omitted, after}` instead of failing with `output_too_large`.
- The conflict check covers what plow-gog covers: each account's shown (selected) calendars plus the calendar being booked, replacing the owned-or-writable rule above.
- An agent guide for `latch google`, ported from the old app's Google skill (`gogSkill.ts`) and rewritten for latch's envelope, exit codes, files and exclusions, ships in the repo and is what `latch google --help` points to.
- gog moves to v0.45.0, after `schema.json` is regenerated and the grammar diff reviewed against the Google list above.

latch keeps its own rules where it is stricter than plow-gog: the exact Google list and exclusions, client-side files instead of inline `@N` files, token handling, the sandbox and fixed error reasons.

Exit codes:

| Code | Meaning | What the agent should do |
|---|---|---|
| 0 | ok | — |
| 1 | operation failed on the host | read the error |
| 2 | usage error, or unknown / ambiguous host | fix the command |
| 3 | host unreachable; nothing was sent | retry later |
| 4 | denied by the host | don't retry |
| 5 | outcome unknown (connection lost after sending) | check before resending |

A path the macOS sandbox refuses is exit 4, code `sandbox_denied`. On Linux a path outside the areas is not in the worker's view at all, so the worker reports what the filesystem reports: a hidden path is `not_found`, exit 1 (§7).

Not in the POC: `--why` (the gatekeeper never trusts what the agent says about itself), `--dry-run`, idempotency keys, jobs, streaming `exec` output, interactive or TTY `exec`, pagination, alternate output formats, the browser and the other tools of today's Latch app.

## 7. Host: `latch serve`

- Runs as the logged-in user (a separate service user is Later). Its state directory (`--state-dir`) holds the key, the Plow token, the audit log and config; the worker can never read it.
- On start: load or create the key, start the iroh endpoint, register (§5.1), then heartbeat on the §5.1 schedule. A changed relay URL triggers an immediate re-register. On a clean shutdown (Ctrl-C, SIGTERM) it calls §5.7 once, waiting at most 2 seconds and ignoring any error, then exits; shutdown never blocks on Plow.
- Accept connections only for latch's versioned ALPN (`plow/latch/0`). Verify the caller (§4) on connect and before each operation.
- **Sandboxed worker.** All file reads and writes, and every `exec`, run in a separate worker process sandboxed by the operating system (macOS Seatbelt in the POC host build; on Linux hosts, bubblewrap plus seccomp, below), applied before the worker does anything: deny by default, write only inside the configured areas, read inside the areas plus the system locations programs need to start (system libraries, `/usr/bin`, `/bin` and similar), and the host's state directory always denied. File workers get no network. `exec` workers get outbound network, so ordinary tools work. **stdin:** without `--stdin` (`-i`) the command's stdin is empty. With it, the client reads its own stdin to EOF before connecting: it exits 2 if stdin is a terminal, if the input exceeds 256 KiB, or if EOF does not arrive within `--timeout`, and never runs partial input. The bytes travel with the request, binary-safe; the host feeds them to the command from a separate task and closes the pipe (a command that exits without reading is normal). The audit records the input's size and sha256, never its content. The wire protocol version is bumped (`plow/latch/1`), so mismatched builds refuse to talk. **`exec` reads follow Codex CLI:** an `exec` worker may read the whole filesystem (what the user can read; macOS privacy protections still apply), on macOS via Codex CLI's Seatbelt base and network rules (vendored, Apache-2.0 notice kept) instead of the read allowlist, and on Linux via a read-only bind of `/`. Its writes stay limited to the areas and its private per-call scratch folder; the state directory stays unreadable and unwritable, and the running `latch` executable is never writable. Apple Events and opening other apps stay denied; on macOS, `exec` gets the named speech and audio services `say` and `afplay` need (the audio component registrar, pluginkit, runningboard, audiohald, and the two MauiAUSP speech XPC services, the only exceptions to the embedded-XPC denial), never a blanket Mach grant; speech works only on a host started in the logged-in (Aqua) session; clipboard services are not granted (Later). The `exec` PATH includes Homebrew (`/opt/homebrew/bin`, `/usr/local/bin`) on macOS. File workers read the same way as `exec` (the whole filesystem, state directory hidden) and still write only inside the areas; Google workers keep the narrow read rules above. `exec` workers get no Apple Events (no AppleScript control of other apps) in the POC. The worker inherits a minimal environment with no host secrets. Areas are the folders named on the command line, `latch serve <folder>...`: the sandbox roots for this run. At least one is required; with none, `serve` exits 2 with a one-line example (`latch serve ~/code`) and never picks a default. Each is resolved to an absolute path and must be an existing directory (exit 2 naming the folder); any directory may be shared, the home directory and `/` included. The state directory is always excluded: when a shared folder contains it, the sandbox hides it from the worker (macOS: denied; Linux: not visible), so it is never readable or writable through latch. The host prints them at startup. There is no host config file: latch reads no `host.json`, and the command line is the host's whole configuration. If the sandbox cannot be applied, the operation fails; it never runs unsandboxed. Paths are resolved (symlinks included) by the sandbox, not by string checks.
- **Linux worker sandbox.** The same policy, built the way Codex CLI builds its Linux sandbox. The host runs the system's bubblewrap (`bwrap`), which builds the worker's filesystem view and namespaces. Every worker sees, read-only, the installed `latch` executable (which bwrap runs again inside the view) and the system runtime and loader paths programs need to start; nothing else is mounted beyond what its kind needs (never the whole root filesystem, unlike Codex's default), the host's state directory is never visible, and `/dev` is minimal. Workers never mount a fresh procfs, which Docker's masked `/proc` paths forbid: a worker either gets no `/proc` or a read-only bind of the existing one, whichever is the narrowest its kind needs; the implementer picks per kind and states the choice in the sandbox module. File workers also see the configured areas read-write and get a network namespace with no network. `exec` workers see the whole filesystem read-only (`/` bound read-only), with the configured areas read-write on top, the state directory hidden and the `latch` executable read-only, and keep outbound network. Google workers see the pinned gog executable read-only and their own per-call folder read-write, no configured areas, and keep outbound network. Inside that view bwrap runs `latch` again, which sets no_new_privs and installs a seccomp filter before it runs the work. This is the only Linux mechanism, with no fallback to another one. Linux workers report whatever the filesystem view gives them, with no separate denied classification: a path outside the areas is hidden, so it is `not_found` (exit 1), not `sandbox_denied` (exit 4) as on macOS. bubblewrap comes from the system, installed by `install.sh` (§9); latch does not bundle it.
- **Startup probe.** Before registering, `latch serve` checks that a worker can really be sandboxed on this machine. If it cannot (on Linux: bubblewrap missing, user namespaces blocked, default Docker, Ubuntu's AppArmor user-namespace restriction), the host refuses to start with a plain message saying what is wrong and what to do (for example, run `install.sh`). When the host detects it is in a container and bubblewrap cannot set up, the message adds a hint of at most four plain lines naming the Docker flags (non-root `--user`, `--cap-drop=ALL`, `no-new-privileges`, the shipped seccomp profile), noting that AppArmor hosts also need a loaded profile, saying not to use `--privileged` or `SYS_ADMIN`, and pointing to "Running a latch host in Docker" (§9). There is no unsandboxed mode. Clients need no sandbox and are unaffected on any OS.
- **Google.** The host gets the tokens (§5.5) and runs a pinned copy of gog (version fixed and verified by sha256 before every run, against the pin compiled into latch from `latch/src/google/pin.json`). The host finds gog at `<state-dir>/plugins/gog/<version>/gog`, where `install.sh` puts it (§9); `plugins/` is the one part of the state directory a worker may see: each plugin's worker sees only its own plugin folder, read-only, and `exec` and file workers see none of it; there is no override. gog runs in the sandboxed worker, with the token passed in the child's environment and nothing else from the host. gog runs with fixed safety flags and exactly the §6 Google list enabled; anything outside the list, any flag that would override the safety flags or redirect gog's network (base-URL or proxy settings), and `--inline` are refused before gog starts. Files the client sends are written into a per-call folder inside the worker and gog's arguments point there; files gog writes go to the same folder, are streamed to the client after gog exits, and the folder is deleted. The token never reaches the worker's output, the client or the audit log.
- **Audit.** An append-only NDJSON file per host. Each record carries the hash of the previous record. Record: time, caller key and agent, operation, arguments (paths, recipients, sizes, sha256 of moved files; for `exec`, argv capped at 4 KiB, workdir, exit code or signal, timed_out and duration; for `google`, the gog command path, flag names, account or accounts, recipients, sha256 of every file sent or downloaded, per-account outcome and the refusal code if refused, but not flag values such as bodies or search queries), decision, outcome. Latch never writes file contents, email bodies, command output or tokens into the audit log itself. The one exemption is `exec` argv, which is recorded verbatim (capped at 4 KiB) whatever it contains, because the command that ran is the thing the audit must show; an agent that puts a secret in argv puts it in the host's local audit log. The audit log stays on the host. Every allow and every deny is recorded.
- Several hosts may run on one machine (separate `--state-dir`s, separate keys, distinct names); the name is always chosen at `login`, never derived from the hostname.

## 8. Transport and wire protocol

- iroh 1.x with n0's public relays by default for the POC (development use). No publishing to n0's DNS discovery: a client dials a host using only the key and relay URL from the Plow directory.
- The host uses n0's public relays; choosing other relays is Later (§10). The host registers its home relay with Plow (§5.1), and a client uses only that relay to reach the host, so a client contacts no relay the host did not choose.
- One bidirectional QUIC stream per operation. Length-prefixed JSON frames: a request frame (`op`, `args`), raw file bytes on the same stream where the operation moves a file, and a final response frame (result or error code). The frames do not depend on iroh, so another transport can carry them later.
- A connection drop after the request was sent maps to exit 5; failure to connect maps to exit 3.

## 9. Repository, build and CI

- A new top-level Rust workspace in the plow monorepo, with its own path-scoped CI job that builds, lints and tests on ubuntu and macOS runners.
- Client builds: macOS (arm64, x86_64) and Linux (amd64, arm64) binaries that run on Debian and Alpine. latch need not be a single static binary: it ships with an `install.sh`. Host builds: macOS only in the POC, because the worker sandbox is Seatbelt; Linux hosts follow right after (§10).
- Docker hosts: the latch workspace ships a Docker seccomp profile (Docker's default profile plus only what non-root bubblewrap needs: `clone` with a new user namespace, `unshare(CLONE_NEWUSER)`, `mount`, `pivot_root` and `umount2` with `MNT_DETACH`) and a short doc, "Running a latch host in Docker". The doc gives the `docker run` flags (`--user` non-root, `--cap-drop=ALL`, `--security-opt=no-new-privileges=true`, `--security-opt=seccomp=<profile>`), a compose snippet, and a Kubernetes note marked untested. It never recommends `--privileged`, `SYS_ADMIN` or `seccomp=unconfined`. On Ubuntu Docker hosts AppArmor's `docker-default` profile blocks bubblewrap's mounts; the doc says so, marks this unverified, and offers a candidate AppArmor profile marked untested. Validating AppArmor and Kubernetes is out of scope for the POC.
- `install.sh` (POC scope): installs a given `latch` binary to a location in the user's home directory. On Linux it also installs bubblewrap through the system package manager (apt, dnf, apk or pacman), asking for sudo once, and installs and loads the `bwrap-userns-restrict` AppArmor profile, as Codex CLI documents, when `/proc/sys/kernel/apparmor_restrict_unprivileged_userns` is `1` and that profile is not already loaded (the default on Ubuntu 24.04 and later). It decides by that setting, not by checking the OS version. On macOS there are no other prerequisites. On every OS it also installs the pinned gog: it reads the version and hashes from `latch/src/google/pin.json` (the same pin latch compiles in, the single source of truth), downloads that gog release for the OS and architecture, checks the extracted binary's sha256 against the pin, and installs it at `~/.latch/plugins/gog/<version>/gog` (the default state directory; `install.sh` takes the state directory as an option for dev installs). The version is in the path, so a latch upgrade that bumps the pin installs the new gog beside the old one and never overwrites it; old versions are left in place. When a gog with the right hash is already at that path it downloads nothing. It is plain POSIX `sh` using `curl`, `tar` and `sha256sum` or `shasum`, with no Python and no new dependencies. It downloads no latch binary (installing bubblewrap and the AppArmor profile through the package manager, and downloading the pinned gog release, are allowed); release hosting and downloads of latch are Later.
- No release pipeline, signing or notarization in the POC; binaries are built from source or downloaded from CI artifacts.

## 10. Fast follow and Later (designed for, not built)

**Fast follow, right after the POC:** Linux hosts, with the bubblewrap + seccomp worker sandbox (§7) as a second implementation of the sandbox module (Global constraints), so a stock Raspberry Pi OS machine can be a host. Chunk 8 is a stretch chunk for it; Chunks 10 and 11 build its sandbox and `install.sh`.

Later:

- Any peer as host: agents accepting connections, an owner's computer calling an agent, agent-to-agent calls, Plow dialing an agent's host to deliver messages.
- `latch doctor` reporting which sandbox layers a host supports.
- No Plow call per request: hosts and clients hold a WebSocket to Plow that pushes peer changes (added, removed, revoked, moved), so lookups and caller checks come from a local copy instead of a Plow round trip each time; it can also replace the heartbeat for presence (Plow already tracks relay-device presence over WebSockets with Redis).
- Pre-warmed single-use workers (one already-sandboxed worker waiting per kind), if worker startup ever shows up as real latency; today Plow round trips dominate.
- Explicit per-folder access on `serve`: read-only vs read-write folders and owner-chosen excludes (e.g. `~/.ssh`), beyond the always-hidden state directory.
- Reading macOS Messages (`chat.db`) through a dedicated read-only command, with Full Disk Access granted to a signed `latch` started by launchd; never through `exec`.
- Open, pending the chef's discussion: a sandbox-free host mode for containers where bubblewrap cannot run. Not in scope.
- Release hosting and downloads for `install.sh`.
- Validating Docker hosts under AppArmor (Ubuntu) and Kubernetes.
- A dedicated `_latch` service user on macOS with a per-user worker, a minimal `.app` wrapper for macOS privacy prompts, signing and notarization.
- Scoped device credentials in place of the owner portal token and the broad activation token (a narrower token for text-to-login); proof of key possession at registration; key rotation; per-peer revocation; per-agent and per-command permissions.
- Removing the agent's own connector and relay permissions, and its access to `/v1/connectors/google/run`, so agents cannot route around latch; re-pointing Hermes's send-approval hook from `plow_run_command` to `latch google`.
- Protecting the Google token in gog's environment from other processes running as the same user (needs the `_latch` service user).
- The gatekeeper judging the user's intent from server-recorded message provenance; asking the owner through Plow.
- Self-hosted relays with a Plow access check; owner-verified keys.
- `latch mcp`: a local MCP server on the client machine exposing the same operations as MCP tools, over the same iroh path, identity check and audit, for tools that only speak MCP. Existing agents keep using today's Latch MCP over the relay until they move to the CLI.
- gog commands beyond the §6 Google list, each after a review of its file and network flags; other Google services (Drive, Docs, Sheets, Contacts), which also need new OAuth scopes.
- More commands (browser, Slack, listing files), streaming and interactive `exec`, AppleScript via `exec`, jobs, and the rex-style `api list/describe/call` discovery.

## Chunks

### Global constraints

- Stage: proof of concept on the path to the real product, not throwaway: clean, simple code later work builds on. Owned test accounts and test files only; no real user data, no production users' agents or devices.
- The existing Electron Latch, `/v1/relay/*`, `relay_devices`, `agents`, `/agents/me`, the relay WebSocket and MCP are not modified.
- One model: every call is an iroh connection authenticated by the caller's own key, including on the same machine. No local fast path, no peer-uid identity, no local/remote branches.
- Nothing may assume agents are only clients or hosts only run macOS (§2, §10).
- Bytes moved by `file get`/`put` and `google` (attachments sent and downloaded), email bodies and tokens never appear in client stdout, the audit log or Plow; downloaded files land as local files on the client, reported by path, size and sha256 only. The exceptions are `exec` and `google`, which return the command's own output (for `google`, gog's JSON) to the client by design (§6); that output never goes to the audit log or Plow. The verbatim-argv audit exemption is for `exec` only; `google` audits flag names, not values (§7). `exec` argv is recorded in the host's local audit log (capped, §7) and never sent to Plow.
- The host fails closed: lookup failure, Plow unreachable or sandbox failure all deny.
- Host names are chosen at `login`, unique per account, never derived from the hostname.
- No `--why`, `--dry-run` or local-only shortcuts.
- The API ships before the CLI; the CLI needs no fallback for older APIs.
- No client or host ever calls Plow in a tight loop: every retry of a Plow call uses exponential backoff with jitter and honours `Retry-After`. Background calls to Plow are limited to the §5.1 heartbeat and the single §5.7 call on clean shutdown.
- Rust style. The chef: "prioritize simple primitives. i really crave this. try not to bloat. i dunno rust, but i would love to learn and hope that this code is readable." So: plain structs, enums and functions over traits and generics; use well-established crates rather than hand-rolling (chef: 'i love using libraries when we can'), but only widely used, actively maintained ones — no obscure packages; every dependency is listed in the library audit with its usage numbers; no macro-heavy or clever lifetime code; small files with one job each; short comments that explain the why to a Rust newcomer. Reviewers flag bloat or cleverness as Important.
- Everything macOS-specific (the Seatbelt sandbox, the Apple Events denial) lives in one small sandbox module behind a plain OS-neutral function. Nothing else in the code may be macOS-specific, so the Linux sandbox (bubblewrap + seccomp, §7, §10) is a drop-in second implementation, and is the only Linux one.
- Packaging: latch ships with `install.sh` (§9) and need not be a single static binary. bubblewrap comes from the system, never bundled. A host that cannot sandbox refuses to start (§7); there is no unsandboxed mode.
- Dev and test state: every dev and test run passes `--state-dir` pointing inside the worktree under the gitignored `.dev/`; no run writes files anywhere else in the home directory.
- Test environment: a local Plow stack from this worktree with its DTUs (the Linq twin delivers the activation and login codes); test accounts, portal logins and agent credentials are minted through the API by script. No production Plow, except the Google token mint below. All peers run on the development Mac; the Linux client runs in a `rust:alpine` container pinned by digest, under OrbStack.
- Google testing: hosts read Google tokens through the test-only `google_token_file` setting (§7), filled by a dev script that mints them from production Plow's existing `POST /v1/connectors/gmail/access-token`. The test accounts are the chef's three connected Google accounts. Mail goes only between those three accounts; calendar events have no other attendees; every created item carries a marker and is deleted afterwards. Reads follow one rule: Gmail searches always include the marker in the query, Gmail message, thread, attachment and draft reads (`drafts get`) target an id the test created, and `drafts list` may run but only the test's own drafts are kept in evidence; calendar reads (event lists, search, freebusy and the conflict check) stay inside one reserved test window, a fixed far-future day chosen by the test where every marker event is placed, and never query outside it; metadata-only reads (`google accounts`, the calendar list, the labels list) are unrestricted. Evidence transcripts keep only output from those reads; nothing else is saved. A Google test run is done only when a final marker search across all three accounts (Gmail by marker, calendar within the test window) is empty.
- Refusal checks are ordinary negative cases (exit 4 and the like); no adversarial, escape or malicious testing.
- Linux host testing: a Docker container run with the documented flags and the shipped seccomp profile (never `seccomp=unconfined`, `--privileged` or `SYS_ADMIN`), a default Docker container (to show the refusal), and a stock Raspberry Pi OS machine, run with the chef (it is the chef's machine).

### Chunk 1: Plow peer directory
Implements: §5 of this spec
Interfaces: consumes existing auth (agent credential resolution, owner portal-login detection, session liveness) · produces `POST /v1/latch/peers`, `GET /v1/latch/peers`, `GET /v1/latch/peers/{key}` and the `latch_peers` table
Done when: API tests cover create, idempotent repeat (refreshes `relay_url` and `last_seen_at`), 409 on a key conflict and on a name conflict within and across kinds (an agent taking a device's name and vice versa), 403 for other credentials, a repeat inside 5 minutes with an unchanged `relay_url` performs no database write (shown by a test), list `online` true and false by `last_seen_at`, lookup returning both device and agent peers, and 404 for another account's key and after the registering credential is revoked; one migration head; curl output against a local stack showing all three routes.

### Chunk 2: Rust workspace, identity, `login` and `status`
Implements: §4, §6 (`login`, `status`, `--state-dir`, `--host` resolution, output and exit-code conventions) and §9
Interfaces: consumes Chunk 1's routes · produces the workspace, the key store, the Plow client and the CLI skeleton later chunks extend
Done when: CI job green on ubuntu and macOS; `file` output shows the Linux binaries for amd64 and arm64 (as built they were static musl, which §9 no longer requires); a Linux binary runs `latch status --help` in the pinned `rust:alpine` container; transcript against the local stack of an agent and a host logging in, `status` listing the host, repeat login changing nothing, and `--host` resolution covering flag, `$LATCH_HOST`, the single-listed-host default and the exit-2 case with several hosts listed.

### Chunk 3: Host (`serve`), transport and audit
Implements: §7 (host lifecycle, caller verification, audit) and §8
Interfaces: consumes Chunks 1–2 · produces the running host, the frame protocol and the audit log that Chunks 4–5 add operations to
Done when: transcript of a client reaching a host over iroh with `conn.paths()` (or equivalent) showing a direct path; heartbeat schedule shown with jitter in the host log, and backoff shown when Plow returns errors (no tight loop); a caller key from another account, an unregistered key and a key whose credential was revoked each denied with exit 4 and logged; Plow unreachable denies; the audit chain verifies and a hand-edited record breaks verification; exit 3 when the host is down.

### Chunk 4: File get and put through the sandboxed worker
Implements: §6 (`file get`, `file put`) and §7 (sandboxed worker)
Interfaces: consumes Chunk 3's protocol · produces file operations later commands reuse
Done when: byte-identical round trip (sha256 shown) of a 200 MB binary file each way; a read outside the configured areas, a symlink pointing into the host's state directory and a write outside the areas are each denied by the sandbox and logged; a forced sandbox-apply failure fails the operation; audit records hold path, size and sha256 but no contents.

### Chunk 5: Google via gog
Implements: §5.5, §6 (`google`) and §7 (Google)
Interfaces: consumes Chunks 3–4 · produces Google parity with today's Latch
Done when: against the chef's three connected Google accounts, under the Google testing rules and read rule (Global constraints: marker queries, test-created ids, the reserved calendar window), a parity checklist of every command in the §6 Google list runs through `latch google` with results matching today's `plow-gog`, including `--send-updates all` on calendar writes other than `respond`, the merged fan-out reads and `accounts`; an email with a PDF attachment above 10 MB arrives byte-identical; that attachment downloaded with `gmail attachment` to a client path lands as a local file whose sha256 matches the original, with stdout holding only path, size and sha256; a command outside the list, one excluded command from each excluded group, a safety-flag override, `--inline`, and a base-URL or proxy override are each refused before gog starts with exit 4; a missing `--account` with several accounts connected exits 2 listing them; a gog failure (an invalid message id) exits 1 with `google_failed` and gog's exit code; a timed create inside the test window over a busy marker event on another account exits 4 with `calendar_conflict` and busy times only, and the same create with `--confirm-conflict` books; a fan-out read with one account's token missing or invalid in the token file (no real grant is revoked) exits 0 with that account `failed` in `accounts` and the other's items present; output above 1 MiB, from a read of test-created items only, exits 1 with `output_too_large`; a gog binary with a wrong sha256 is refused; a grep of client output and the audit log finds no token, and the audit log holds no body or query text; a call from a revoked caller is denied before any Google call; and a final marker search across all three accounts is empty.

### Chunk 6: `exec` on the host
Implements: §6 (`exec`) and §7 (sandboxed worker for `exec`)
Interfaces: consumes Chunks 3–4 (protocol, worker) · produces remote command execution
Done when: `latch exec --host H -- uname -a` returns the host's output and exit code 0; a command exiting 3 reports `exit_code: 3` with latch exiting 0; `sh -c` with a pipe works; writing outside the areas and reading the host's state directory are denied by the sandbox; a command printing 10 MB returns 256 KiB per stream with `truncated` true; a `sleep 120` with `--timeout 2` is killed with its children and returns `timed_out: true`, `signal: "SIGKILL"`, `exit_code: null` with latch exiting 1; a command that kills itself with SIGSEGV reports that signal with latch exiting 0; `curl` to a public URL succeeds; an `osascript` call to another app is denied; each run is in the audit log with argv, workdir, exit code or signal, timed_out and duration, and a grep for a marker string the command printed finds it in the client response but not in the audit log.

### Chunk 7: End to end, several hosts
Implements: §1 (all goals together)
Interfaces: consumes Chunks 1–5
Done when: transcript of the Linux client in the pinned `rust:alpine` container reaching two hosts (separate `--state-dir`s under `.dev/`) with different names and different test files, where `--host a` and `--host b` each return the right file, omitting `--host` exits 2 listing both, each host's audit log contains only its own calls, one `latch google` send and one calendar read inside the test window succeed (Google testing rules), one `exec` runs on each host and reports that host's name, stopping one host gives exit 3 for it while the other still works, and every created peer, credential, file and container is cleaned up afterwards, with a final Google marker search across all three accounts empty.

### Chunk 9: Host dashboard and Plow branding
Implements: §6 (`status`, bare `latch`) and §7 (`serve` output); stage unchanged
Interfaces: consumes Chunks 2–3 (and the operations of Chunks 4–6 as they land) · produces the host dashboard, the bare-`latch` logo and `status --pretty`
- `latch serve` shows a live dashboard when stdout is a terminal, with only: the host name, the time of the last heartbeat, calls per minute, and one row per call with its time, the calling agent, the operation and its target, and the result (exit code, size, duration). A refused call may appear as a plain row. No allowed/denied counters, no direct or relay path, no audit-chain status. All of it comes from data the host already has. When stdout is not a terminal, or with `NO_COLOR` or `--no-dashboard`, it logs plainly as before. While the dashboard has the screen, host logs go to a file under the state directory.
- Bare `latch` (no arguments) on a terminal shows the Plow logo with its animation from the old Latch app (reference only, nothing imported), saying "Plow". Nothing showy anywhere else.
- `latch status` keeps JSON as its default output. `--pretty` opts into a human card: identity, the key as OpenSSH-style "drunken bishop" key art, and hosts. It is aimed mainly at hosts, so an owner can compare the key art shown on a host with the one shown on an agent.
- Dashboard and card output never mixes into JSON and honours `NO_COLOR`. Only well-known crates from the library audit (crossterm and ratatui need audit rows first).
- Disposable prototypes of the serve, logo and status scenes to borrow from: `.dev/devex/latch-flair/` in this worktree (gitignored).
Done when: recorded terminal transcripts or screens of the dashboard during real calls (one allowed, one denied), of bare `latch` showing the logo, and of `latch status --pretty`; and piped output of `status` and of the `serve` logs is byte-for-byte unchanged from before this chunk.

### Chunk 8 (stretch): Linux host
As built, with a Landlock sandbox; superseded by Chunk 10, which replaces that backend. Kept as the record of what was built.
Implements: §10 fast follow (Linux hosts) and §7 (sandboxed worker) on Linux
Interfaces: consumes Chunks 3, 4 and 6 · produces the second implementation of the sandbox module and `latch serve` running on Linux
Done when: on a Linux host (an OrbStack container or a Linux VM, with the kernel shown to support Landlock), `latch serve` runs and every Chunk 4 and Chunk 6 Done-when check passes against it, with the sandbox being Landlock for files plus seccomp or a network namespace to cut file workers' network (`exec` workers keep outbound network); the Chunk 6 `osascript` check does not apply, since there are no Apple Events on Linux; nothing outside the sandbox module changes for Linux.

### Chunk 10: Linux bubblewrap sandbox
Implements: §7 (Linux worker sandbox, startup probe) and §10 fast follow
Interfaces: consumes Chunk 8 (and through it Chunks 4, 5 and 6) · replaces the Landlock backend of the Linux sandbox module, which is deleted; nothing outside the sandbox module and the startup probe changes · produces the Docker seccomp profile and the "Running a latch host in Docker" doc (§9)
Done when: in a Docker container run with the documented flags and the shipped seccomp profile (not `seccomp=unconfined`, `--privileged` or `SYS_ADMIN`), with Docker's `/proc` masks left in place, every Chunk 4 and Chunk 6 Done-when check passes (the `osascript` check does not apply; the checks the sandbox denies on macOS instead report what bubblewrap's view gives, so a read of the host's state directory or of a path outside the areas is `not_found`, exit 1), with the host log showing the bubblewrap sandbox, a file worker shown to have no network and an `exec` worker reaching a public URL; under the Google testing rules (Global constraints: the chef's accounts, marker items, the reserved test window, cleanup), one `latch google` marker send between two of the chef's accounts, one marker search finding it and one calendar read inside the test window succeed through the bubblewrap Google worker, and a final marker search across all three accounts is empty; in a default Docker container `latch serve` refuses to start, exits non-zero and prints a plain message naming the cause and what to do, followed by the Docker hint (§7), and no worker runs; the same refusal when `bwrap` is not installed; no Landlock code or crate remains; refusal checks are ordinary negative cases only.

### Chunk 11: `install.sh`
Implements: §9 (`install.sh`)
Interfaces: consumes Chunk 10 (a built `latch` binary with the bubblewrap sandbox) · produces `install.sh` in the latch workspace
Done when: on macOS (with a temporary `HOME`) it installs the given binary into the user's home directory and the pinned gog at `~/.local/share/latch/gog/<version>/gog`, and installs nothing else; in Debian and Alpine containers it installs the pinned gog the same way; a rerun with gog already correct downloads nothing; after the pin is changed to another version, a run installs that version beside the old one and leaves the old one untouched; a tampered gog at the default path is refused by latch with `gog_unverified`; `latch google` works on the Mac host with no `gog_path` in `host.json`; in Debian, Fedora, Alpine and Arch containers it installs the binary and bubblewrap with that system's package manager, asking for sudo once (or running as root without sudo in the container), and running it again changes nothing; on Ubuntu 24.04, where `/proc/sys/kernel/apparmor_restrict_unprivileged_userns` is `1` and the profile is missing, it installs and loads the `bwrap-userns-restrict` AppArmor profile, and a second run, with the profile now loaded, leaves it alone; on a system where that setting is absent or `0` it does not touch AppArmor; it downloads no latch binary; Pi acceptance for Chunks 10 and 11 together: on the chef's stock Raspberry Pi OS machine, with the chef, `install.sh` installs latch, bubblewrap and the arm64 pinned gog (shown installed and its sha256 matching the pin; no Google call is made on the Pi), `latch serve` passes its startup probe and starts, and a Linux or Mac client runs one `file get`, one read of the host's state directory, which reports `not_found` (exit 1), and one `exec`; every container used is removed afterwards.

### Chunk 12: Text-to-login and device management
Implements: §4 (login by text), §5 (owner login, the widened §5.2 list, §5.6) and §6 (`login`, `devices`, `devices remove`)
Interfaces: consumes Chunks 1 and 2, existing device activation (`POST /v1/auth/activate`, `/activate/redeem`) and session revocation · produces the latch router's acceptance of the activation token, the widened `GET /v1/latch/peers`, `DELETE /v1/latch/peers/{key}`, and the `login` and `devices` client changes; there is no `/v1/latch/devices` route; `auth.py` and every route outside `/v1/latch` are unchanged
Done when: API tests cover the activation `*:*` token registering a `device` peer and passing §5.2 and §5.3 while `is_portal_login` still rejects it elsewhere; `GET /v1/latch/peers` listing devices and agents, with and without a `relay_url`, online and offline, for both an owner login and an agent, and still excluding peers whose registering credential is no longer live; `DELETE /v1/latch/peers/{key}` deleting a device peer and revoking its registering session; an agent credential getting 403 on delete; 409 `not_a_device` on an agent peer; 404 for another account's key; 409 `own_machine` when the caller tries to remove its own machine; and a removed host's next heartbeat failing and not re-registering; a latch transcript against the local stack where `login` prints the activation text, the Linq twin's `POST /ui/inbound` sends it, and `login` completes and registers, with a grep showing the code in no log; the name-conflict prompt answered yes (the old machine removed and signed out) and no; a repeat `login` on a machine already logged in by text uses the saved owner token, starts no activation (no `POST /v1/auth/activate` in the stack's log) and leaves its Plow session unchanged (same session, not revoked, no new one); `latch status` and `--host` resolution still listing only peers with a `relay_url` while `latch devices` lists every `device` peer (a device without a `relay_url` included) and no agents; the non-terminal paths (no credential → exit 2 naming `--token-file`; name taken → exit 2 naming `latch devices remove <name>`; `devices remove` without `--yes` → exit 2); then on the chef's Raspberry Pi over `ssh rpi5ld`, with the chef: login by text through the twin, the state directory wiped, login again taking over the name, and `latch devices` and `latch devices remove` shown; every created peer, login and file cleaned up afterwards.

### Chunk 13: Going offline on clean shutdown
Implements: §5.7 and the clean-shutdown rule in §7 of this spec
Interfaces: consumes §5.1 registration · produces `POST /v1/latch/peers/{key}/offline`
Done when: API tests cover the registering credential marking its peer offline (§5.2 then shows `online: false`), a repeat call returning 200, another credential in the same account and another account each getting 404, and the next §5.1 call writing `last_seen_at` again; then on the local stack a transcript where `latch serve` is stopped with Ctrl-C and `latch status` from a second state dir shows it offline within seconds, restarting shows it online, and a serve stopped with Plow unreachable still exits within about 2 seconds. Plow API changes stay local on the `latch-cli` branch: no PR until the head chef asks.

### Chunk 14: Same login takes over its own name
Implements: the same-session exception in §5.1 of this spec
Interfaces: consumes §5.1 and §5.6 · produces nothing new for later chunks
Done when: API tests cover a device peer re-registered under its name with a new key and `take_over` by the same session (200, one peer, new key), the same without `take_over` getting 409, the old key's next registration then getting 409 (no taking it back), a different session still getting 409, and an agent peer never re-keyed this way; `latch login --name <taken by this login>` on the local stack completes with no replace prompt and prints that the name now points to this machine, a `latch serve` still running on the old key logs the 409 and exits non-zero with a message saying the name moved to another machine, while a name taken by a different login still prompts as before. Plow API changes stay local on `latch-cli`: no PR until the head chef asks.

### Chunk 15: Shared folders as `serve` arguments
Implements: the areas rule in §7 of this spec
Interfaces: consumes the existing worker sandbox · produces `latch serve <folder>...`
Done when: on macOS and in the Linux container, `latch serve <dir>` with no `host.json` serves `file get`/`exec` inside `<dir>` and denies outside it as before; two folders both work; no folder, a missing folder and a file each exit 2 with a plain message; sharing the home directory (with the state directory inside it) works, and `file get`, `file put` and `exec` (`cat`, `ls`, writing a file) on the state directory and its files all fail on macOS and Linux; latch reads no `host.json` (grep shows no reader left) and `gog_path`, `google_token_file` and `relays` are gone; the startup output lists the folders; tests, clippy and the existing sandbox tests pass; the chef's quickstart updated to the new form; `.dev/bin/latch` and the Pi copy rebuilt.

### Chunk 16: Codex-style `exec` sandbox
Implements: the `exec` reads rule in §7 of this spec (macOS and Linux)
Interfaces: consumes the existing worker sandbox and areas · produces nothing new for later chunks
Done when: on macOS, through a real `serve` + `exec`, `git status`, `python3 -c`, `swift --version`, `xcrun --find clang`, bare `brew --version`, `defaults read` and `curl` succeed, and on Linux (container and the Pi) `git`, `python3` and `curl` succeed where installed; on both, reading a file outside the areas via `exec` works while writing outside the areas, reading or writing the state directory and overwriting the `latch` executable all fail; `file get` outside the areas is still denied; the dashboard row marks a call whose stderr is non-empty; tests and clippy pass; `.dev/bin/latch` and the Pi dev build rebuilt.

### Chunk 18: `exec --stdin`
Implements: the stdin rule in §7 of this spec
Interfaces: consumes `exec` · produces `latch exec --stdin`/`-i` and protocol `plow/latch/1`
Done when: on macOS and Linux hosts, `cat <<'EOF' | latch exec --host H -i -- sh` runs the script, and `latch exec -i -- sh < script.sh` too; binary input round-trips through `exec -i -- cat` by sha256 (via a remote file); without `-i`, `echo hi | latch exec -- cat` gives empty stdout; `-i` from a terminal, input over 256 KiB, and a pipe held open past `--timeout` each exit 2 without running anything; a command that never reads stdin still finishes; the audit shows size and sha256, not content; a client and host on different protocol versions refuse cleanly; help updated; tests and clippy pass; builds rebuilt (`.dev/bin/latch.new` if the chef's serve runs).

### Chunk 19: gog as the first plugin
Implements: the plugin path rule in §7 and §9 of this spec
Interfaces: consumes the Google worker · produces `<state-dir>/plugins/<name>/<version>/`
Done when: gog is found only at `<state-dir>/plugins/gog/<version>/gog`; the Google worker can run it while `exec` and file workers cannot read anything under the state directory (plugins included); `install.sh` installs gog there (default and dev state dir); the chef's Mac dev home and the Pi dev home have gog installed there and `latch google --host H -- gmail labels list` works against prod on both; the old `~/.local/share/latch` gog removed from the Pi; tests and clippy pass.

### Chunk 20: plow-gog parity
Implements: "plow-gog parity" in §6 of this spec
Interfaces: consumes `latch google` and the plugin path · produces the parity behaviour, the Google agent guide and gog v0.45.0
Done when: tests cover each parity bullet against plow-gog's behaviour for the same argv (a shared argv → expected-plan list taken from plow-gog's tests); against prod on the chef's Google accounts (read-only, plus marker-titled test events deleted afterwards), a fan-out `calendar events` shows the compact owner-zone shape, `--account` placed after `--` works, and a timed create over a shown read-only calendar's busy time is refused then booked with `--confirm-conflict`; the gog grammar diff from 0.36.0 to 0.45.0 is written up; tests and clippy pass; builds rebuilt (`.dev/bin/latch.new` if the chef's serve runs, and the Pi dev build).
