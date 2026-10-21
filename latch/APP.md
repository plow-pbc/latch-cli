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
{"state_dir":"/Users/me/.latch","version":"0.1.0 (commit 85469a3b2, built 2026-10-21 04:43 UTC, local build, protocol plow/latch/1)","protocol":1,
 "peer":{"name":"studio","key":"d9438ef1…","kind":"device"},
 "serve":{"running":true,"ready":true,"state":"serving","pid":63632},
 "shares":["/Users/me/work"],"last_error":null}
```

Field by field:

- `peer` is `null` before login.
- `serve.running` means a `latch serve` holds the state folder's lock.
- `serve.ready` means that serve is online and accepting callers.
- `serve.state` is one of `starting`, `serving`, `stopped` or `error`.
- `last_error` says why the host last stopped, when it stopped because of a failure. A host that was killed or crashed shows `serve_ended`.

## `latch shares` and `latch shares set FOLDER…`

These show or replace the folders `latch serve` shares when it is started
with no folders, so a service starts the host the way a person set it up.

- Folders named on the command line still win over the saved ones.
- A running serve keeps its folders until it is restarted.
- Each folder must exist. Each is saved as its real, absolute path.

```
{"shares":["/Users/me/work"]}
```

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

`--lifecycle-only` leaves the activity out. That is the menu bar's subscription.
