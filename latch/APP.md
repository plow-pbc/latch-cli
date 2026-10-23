# latch for an app

Four commands let an app (the Mac app) drive latch without a terminal.
Each prints JSON. Each reads only this machine's state folder, except for
`login`, which talks to Plow. None of them prints a token.

The streams among them (`login --events`, `status --local --follow`,
`audit`, and also `logs`) print one JSON object per line, each with a `type`. The first
line is always `{"type":"hello","schema":1}`: the schema the lines follow.
Ignore fields and line types you do not know. `--pretty` never changes a
stream's lines.

## `latch login --events`

This logs in by text and reports each step as one JSON line on stdout.
Without `--events`, a login by text needs a person at a terminal; with
`--events`, it doesn't. To cancel, kill the process. If the state folder
already holds a login, the command goes straight to `verified`.

```
{"type":"hello","schema":1}
{"type":"code","code":"JFO3S","send_to":"+15550000001","send_to_display":"+1 555-000-0001","smsto":"SMSTO:+15550000001:JFO3S","sms_link":"sms:+15550000001?&body=JFO3S","expires_at":null}
{"type":"waiting"}
{"type":"verified","peer":{"name":"studio","key":"d9438ef1…","kind":"device"}}
```

The `code` line carries everything the app shows:

- `smsto` is the QR code's payload, exactly as the terminal's QR code holds it.
- `sms_link` opens Messages with the text filled in.
- `expires_at` is set only when Plow sends one.

A login can also end two other ways:

- `{"type":"expired"}` means the code ran out before anyone texted it. It exits 1.
- `{"type":"error","error":{"code":"…","message":"…"}}` covers anything else, with the usual exit code.

In both cases, the usual error JSON also goes to stderr.

When another of your machines already has the name, the error is
`conflict` (exit 2), and it says which machine has it, for the app to ask
"Use another name" or "Replace studio":

```
{"type":"error","error":{"code":"conflict","message":"a machine named \"studio\" already exists…","existing_name":"studio","last_seen_at":"2026-10-21T04:44:15Z"}}
```

`last_seen_at` is null for a machine that was never seen. To replace it,
run the login again with `--replace` (and the same `--name`): the other
machine is removed from your account, which signs it out of Plow, and this
one takes its name. The token already saved is reused, so no second text
is needed. Pass `--replace` only when the person chose it. An agent's name
is never taken over.

## `latch status --local`

This reports what this machine knows about its own latch, asking no one. It
works offline and before login. Not being logged in, or not serving, is a
normal answer, not an error.

```
{"login":{"kind":"device"},
 "this_machine":{"name":"studio","key":"5b66c591…","hosting":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":95728,"blocked":null,"deny_exec":{"every_agent":false,"agent_uids":[]}}},
 "plugins":[{"name":"google","state":"enabled"},…],
 "update":{"available":false,"build":null,"checked_at":null},
 "last_error":null,"api_url":"https://api.plow.co","state_dir":"/Users/me/.latch",
 "version":"0.1.0 (commit b0be31f8b, built 2026-10-23 19:40 UTC, local build, protocol plow/latch/4)","protocol":4}
```

Field by field:

- `login` is `null` before login, else `{"kind": "device" | "agent"}`. Logged in means `login` is set; don't decide it from a name.
- `this_machine` is the machine whose status this is. Its `name` and `key` are the ones `serve` registers, `null` before login.
- `this_machine.hosting.running` means a `latch serve` holds the state folder's lock.
- `hosting.ready` means that serve is online and accepting callers.
- `hosting.managed` means it runs as this user's service, as `latch serve` and `serve -d` start it, not `serve --foreground`. An upgrade stops a host `serve -d` started and starts it again with `latch serve -d --again`.
- `hosting.attached` means a managed host is held by the `latch serve` in a terminal that started it. An upgrade stops it, and a host run with `serve --foreground`, and says to run `latch serve` there again.
- `hosting.blocked`, when set, says why a running host cannot be used: `other_version`, a host from another latch version, with its pid and how to stop it. `hosting.ready` is then false.
- `hosting.state` is one of `starting`, `serving`, `stopped` or `error`. A machine that never served says `stopped`.
- `plugins` is what `latch plugins list` says.
- `last_error` says why the host last stopped, when it stopped because of a failure. A host that was killed or crashed shows `serve_ended`.

## `latch status`

The same as `status --local`, plus what Plow says of the account:

```
{"account":{"owner_uid":"9a51a542-…","display_name":"Morgan"},
 …the fields of status --local…,
 "hosts":[{"name":"studio","key":"5b66c591…","self":true,"online":true,"last_seen_at":"2026-10-23T19:41:50.229474Z"},
          {"name":"pi-kitchen","key":"ea6a25b3…","self":false,"online":true,"last_seen_at":"2026-10-23T19:41:58.723159Z"}],
 "default_host":"studio","default_host_reason":null}
```

- `hosts` are the account's hosts as Plow lists them, this machine first; `self` marks it. A machine that never served isn't one. `online` and `last_seen_at` are Plow's report, as Plow names them.
- `default_host` is where a command without `--host` goes. When there is none it is `null`, and `default_host_reason` says why: `unknown_host` (`$LATCH_HOST` names no host), `no_host`, `host_required` (several, and none chosen) or `not_logged_in`.
- Before login it asks no one: `account`, `hosts` and `default_host` are `null`.
- When Plow can't be asked it fails, with `plow_unreachable` (exit 1, `next` `latch status --local`) or `plow_denied` (exit 4); it never answers with Plow's fields left out.

## Exec and file permissions

Starting `latch serve` needs no folder configuration for exec or file.
They read home and system files, except `~/.latch`, this host's state directory,
and on macOS `~/Library` except `~/Library/Caches`. Other home content, including
credential dotfiles, is readable; each use is judged.

Exec declares repeated `--write PATH` grants and opts into IP networking with
`--network` (off by default). Its private temp and existing plain tool-cache
folders are writable; missing or symlinked caches are skipped, never created.
Workdir defaults to the first write folder, otherwise home. Writes outside
home, to home itself or to hidden places are refused. Host Unix sockets remain
blocked even with network; macOS permits its system DNS resolver.

File get reads a readable path; file put writes only its judged destination.
Quote leading `~` to expand it on the host. Program argv stays literal. The
remote judge and sealed audit see argv, workdir, writes and network; keep
credentials out of argv.

## `latch status --local --follow`

This prints `{"type":"hello","schema":1}`, then `{"type":"status","status":{…}}`
with what `latch status --local` says, and again whenever that changes: the
host starting, serving, stopping or failing, a login, an update found. It
sleeps when nothing changes, never opens the audit, and works before login
and with the host stopped. When stdin is a pipe, it exits once that pipe
closes, so an app that dies leaves nothing behind.

## `latch audit`

```
latch audit [--last N | --all] [--before ID]
latch audit --follow [--after ID | --last N | --all]
```

This reads the host's audit log, as JSON lines, every form alike:

```
{"type":"hello","schema":1}
{"type":"snapshot","cursor":"<id before the first record shown>","has_older":true}
{"type":"record","cursor":"27ff057f…","record":{"id":"27ff057f…","time":"2026-10-21T04:44:15.488378Z","caller":"b735f084…","caller_name":"laptop","agent":null,"op":"exec","decision":"allow","outcome":"ok","details":{"started":"…","args":{"argv":["echo","hi"],…},"gate":null}}}
{"type":"caught_up","cursor":"27ff057f…","has_older":true}
```

- It shows the newest 50 records, oldest first; `--last N` shows N, `--all` every one.
- "Load earlier" is `--before` the first record's id: the page before it, which `has_older` says exists.
- With `--follow`, each new record follows `caught_up` as it is written. To pick up where you left off, `--follow --after` the last `cursor` you got: every record after it, then the new ones. Nothing is missed or repeated, across host restarts too.
- `caller_name` is the caller's name as it was when it called: an agent's name in Plow, else its machine's (null on records from before latch kept it).
- A record's id is the `audit_id` its caller got back. The cursor before the first record is 64 zeros.
- Don't treat a page as complete before `caught_up`.
- Without `--follow` it ignores stdin and always prints the whole page. With `--follow`, it exits once stdin (a pipe) closes, at any point, even in the middle of the opening.
- Each record is opened with the state folder's key on the host, so the app never holds the key. Its `details.args` hide credential-looking values, as the dashboard does: best effort, some can slip through.

Errors:

- `cursor_expired` (exit 2): `--after` or `--before` names a record the log doesn't hold. Read it again without them.
- `usage` (exit 2): flags that don't go together, `--last` below 1, or an id that isn't one (64 hex characters).
- `no_host_here` (exit 2): this machine has never served, so it keeps no audit; read a host's with `--host NAME`.
- `audit_broken` (exit 1): the log fails its checks, or no longer begins with what a follower was sent.
- A stream can end with one error on stderr after valid lines. Keep the last `cursor` you got.

`latch audit verify` (the chain, checked without the key) and `latch audit
rotate-key` are upkeep, on this machine only.

## Another host: `--host NAME`

`latch status --host NAME [--follow]` and `latch audit --host NAME [...]`
read another of the owner's hosts over latch's connection and print exactly
what that host's own `status --local [--follow]` and `audit` print there. Only the owner's own devices may
read them; an agent's login gets exit 4 `owner_only`. Naming this machine
reads it here.

A host on an older latch answers `status --host` in that latch's shape
(`peer` and `serve` rather than `login` and `this_machine`) until it is
updated with `latch update` there.

- An offline host gives exit 3 `host_offline` at once; nothing is shown from
  elsewhere.
- Each read is recorded in that host's audit, as `status` or `audit`, when
  it ends.
- Each line travels in a frame of its own. A record too large for one (16 MiB)
  ends the read with exit 1 `record_too_large`, its `id`, `size` and `limit`
  in the error; it is never left out.
- A followed stream ends when stdin (a pipe) closes, on Ctrl-C or SIGTERM,
  closing the connection so the host stops reading too. If that host stops,
  it ends with exit 3 `host_stopped`; a dropped connection with exit 3
  `host_unreachable`. Either way, run it again (with `--after` for the audit).

## Calling this machine's receiver

Run ordinary `latch exec`, `latch file`, `latch browser` and `latch google` commands on the
host's state folder. A saved host resolves locally, even while Plow is
unreachable; explicitly naming this machine with `--host` also resolves
locally. The CLI connects to `<state-dir>/serve-v4.sock`, inside the private
state folder, mode 0600. It never dials its own iroh key.

The socket accepts the same length-prefixed `Request` and `Response`
frames as the network, including file-transfer bytes, one operation per
connection. The receiver establishes the caller as `owner_on_host`; a
request cannot choose its caller identity. Owner-only vault and rules
operations also work over the socket, without asking the model. The app
can use the CLI rather than implement this protocol itself.

A stopped receiver gives exit 3 `serve_required`, with instructions to
start `latch serve`. Plow or model outages give exit 3
`gatekeeper_unavailable` for gated work. Local configuration commands
continue to edit state directly, with the same file locks as `serve`.
