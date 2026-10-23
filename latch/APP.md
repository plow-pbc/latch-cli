# latch for an app

Four commands let an app (the Mac app) drive latch without a terminal.
Each prints JSON. Each reads only this machine's state folder, except for
`login`, which talks to Plow. None of them prints a token.

## `latch login --events`

This logs in by text and reports each step as one JSON line on stdout.
Without `--events`, a login by text needs a person at a terminal; with
`--events`, it doesn't. To cancel, kill the process. If the state folder
already holds a login, the command goes straight to `verified`.

```
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

## `latch status --local`

This reports what this machine knows about its own latch, asking no one. It
works offline and before login. Not being logged in, or not serving, is a
normal answer, not an error.

```
{"state_dir":"/Users/me/.latch","version":"0.1.0 (commit 85469a3b2, built 2026-10-21 04:43 UTC, local build, protocol plow/latch/4)","protocol":4,
 "peer":{"name":"studio","key":"d9438ef1…","kind":"device"},
 "serve":{"running":true,"ready":true,"configuring":false,"managed":true,"state":"serving","pid":63632},
 "last_error":null}
```

Field by field:

- `peer` is `null` before login.
- `serve.running` means a `latch serve` holds the state folder's lock.
- `serve.ready` means that serve is online and accepting callers.
- `serve.managed` means it runs as this user's service, as `latch serve` and `serve -d` start it, not `serve --foreground`. An upgrade stops a host `serve -d` started and starts it again with `latch serve -d --again`.
- `serve.attached` means a managed host is held by the `latch serve` in a terminal that started it. An upgrade stops it, and a host run with `serve --foreground`, and says to run `latch serve` there again.
- `serve.blocked`, when set, says why a running host cannot be used: `other_version`, a host from another latch version, with its pid and how to stop it. `serve.ready` is then false.
- `serve.state` is one of `starting`, `serving`, `stopped` or `error`.
- `last_error` says why the host last stopped, when it stopped because of a failure. A host that was killed or crashed shows `serve_ended`.

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

## `latch events [--follow] [--after CURSOR] [--lifecycle-only]`

This reports what the host on this state folder is doing.

It always starts with a snapshot: the host's state and its recent activity,
oldest first (up to 50 records). With `--follow`, it then prints each change
as it happens. It sleeps when nothing changes: no polling, no timers. When
stdin is a pipe, it exits once that pipe closes, so an app that dies leaves
no `events` behind.

```
{"type":"snapshot","cursor":"64617-1792557851878:2:27ff057f18f54d04","host":{"state":"serving"},"activity":[…]}
{"type":"host","cursor":"…","state":"stopped"}
{"type":"activity","cursor":"…","entry":{"id":"27ff057f…","time":"2026-10-21T04:44:15.488378Z","caller":"b735f084…","caller_name":null,"agent":null,"op":"exec","decision":"allow","outcome":"ok"}}
```

Activity entries:

- Each entry is a verified audit record, without its arguments. Connections are recorded too, as `op: "connect"`.
- The audit log is sealed on disk; `latch events` opens each record with the state folder's key, so the app never holds the key.
- An entry's `id` is the record's hash, which is the `audit_id` the caller got back. It is stable across replays.

Host lines:

- A host line in the `error` state also carries `error` (a message) and `error_code`.

Cursors:

- Treat a cursor as opaque.
- `--after CURSOR` picks up right after that line. The snapshot then holds exactly the records since it: none missed, none repeated.
- A cursor that can't be honoured gets `{"type":"reset"}` and then a fresh snapshot. The app replaces what it holds instead of merging. Three cases:
  - the host restarted since the cursor;
  - the audit log no longer matches it;
  - it is more than 1000 records behind.

A follower that stays connected while the host restarts gets the same
`reset` and a fresh snapshot. Every record is checked the way the host
checks its log (each record's own hash, and the chain from the first). A
record that fails those checks is never sent: the stream starts over if the
log was replaced by a sound one, and otherwise stops with `audit_broken`.

`--lifecycle-only` leaves the activity out. That is the menu bar's subscription.

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
