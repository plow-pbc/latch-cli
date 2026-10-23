# Google through latch

A host runs gog ([openclaw/gogcli](https://github.com/openclaw/gogcli), MIT
licensed), the release pinned in `src/google/pin.json`. latch downloads it,
it does not ship it: `install.sh` fetches the release for the machine's OS and
architecture, checks the binary's sha256 against the pin, and puts it in the
state directory's plugins folder, `<state-dir>/plugins/gog/<version>/gog`
(`~/.latch/plugins/…` by default; `sh install-binary.sh --state-dir DIR latch` for
another state directory). The host looks for it there and nowhere else. Run
`install.sh` again after a latch upgrade that changes the pin; the new gog
goes into its own version folder beside the old one.

`plugins/` is the only part of the state directory a worker may see: the
Google worker reads and runs its own gog folder, read-only, while `exec` and
file workers can neither read nor run anything under the state directory.

Google is off on a host until its owner runs `latch plugins enable google`
there, with or without `latch serve` running: the setup checks that gog is installed
and that Plow has a Google account connected for this host to use. With
none connected, it gets a single-use Google sign-in link from Plow (it
works once, for about 2 minutes): at a terminal it shows the link and a QR
code and waits up to 5 minutes for the account, then turns Google on;
otherwise it fails with `google_not_connected`, the link in `connect_url`.
`--events` reports the link, then the result, as JSON lines for an app. The
link is a credential until used: latch never writes it to the audit or a
log. Until then every Google call gets `not_enabled`. `latch plugins disable google`
only stops this host asking Plow for Google credentials; nothing is revoked
at Google or Plow, and other hosts keep their access.

The host checks gog's sha256 against the pin before every run and refuses a
gog that does not match (`gog_unverified`). Every Google call gets a unique OS
temp folder with mode 700, granted only to its worker. The host state
directory stays denied to all workers.

```sh
latch google --host my-mac -- accounts
latch google --host my-mac -- gmail search 'subject:example'
latch google --host my-mac --account me@example.com -- gmail send \
  --to someone@example.com --subject Example --body-file ./body.txt --attach ./report.pdf
latch google --host my-mac --account me@example.com -- gmail attachment MESSAGE ATTACHMENT --out ./report.pdf
```

Agents should read [GOOGLE-AGENT-GUIDE.md](GOOGLE-AGENT-GUIDE.md), which is
built into latch: `latch google --help` prints it. latch google copies the opinions of plow-gog
(the old Latch app's planner, which Plow's Google route also runs), keeping its
own stricter command list, files, token handling and sandbox;
`src/google/parity.rs` runs plow-gog's test argv through latch and says where
latch differs.

`--account` (a comma list allowed) and `--confirm-conflict` may come before
`--` or anywhere among gog's arguments. Gmail search and calendar
calendars/events/freebusy/conflicts merge the requested accounts, or all
connected accounts, running at most four at once; an account the owner
connected without that kind of read (Plow's token answer gives each
account's `capabilities`) is skipped with reason `no_access` instead of run.
Other operations need one account. Calendar event lists read `--max 100` and use the host's time zone
(`--timezone`) unless the agent chose; merged ones come back compact
(`summary`, `startDayOfWeek`, `startLocal`, `endLocal`, `allDay`,
`attendees`, `transparency`, `declined`, `id`, `account`) and are cut at the
last whole event past 40,000 characters, with `truncated: {omitted, after}`.
File arguments refer to the client's files. Downloads report client paths,
sizes and SHA-256, and transfer the bytes separately. Calendar
create/update/move/delete supply `--send-updates all` unless a mode was
supplied. Upstream gog's `respond` verb has no notification-mode flag.

Timed creates (any create but `--all-day`) check busy time on every calendar
the owner shows (`selected`) on every connected account with Calendar
access (Plow's token answer says which), read-only ones included, plus the calendar being booked (probed even when the account shows
none). An account whose calendar list answered with nothing shown has nothing
to be busy on and counts as checked; one whose list failed does not. Busy or incomplete checks refuse with
times and unchecked account names. After the owner agrees, repeat the command
with `--confirm-conflict`. That submission is checked again. A calendar Google
answers `notFound` for (no access, as with a subscribed holiday calendar) does
not block the booking; the created event's answer names it in
`could_not_check`. Any other per-calendar error, or an account where nothing
answered, makes the check incomplete.

The host obtains short-lived credentials from Plow for each operation. A Google operation has a host deadline of 90 seconds, leaving 30 seconds
inside the client answer window. Expiry kills the worker and gog together.
A read timeout exits 1; a write timeout exits 5 because its outcome is unknown.
Check Google state before resending a timed-out write.
Follow the repository's Google testing restrictions when using real
accounts: marked fixtures only, the reserved calendar day, and final empty
marker searches on every account.

`src/google/schema.json` is the pinned CLI's grammar trimmed to the reviewed
Gmail/Calendar commands, aliases and flags. Regenerate it with:

```sh
python3 latch/scripts/google-schema.py /path/to/pinned/gog latch/src/google/schema.json
```

Review changes to the grammar and file flags together with `policy.rs` when
upgrading gog. The script drops flags latch refuses: `--inline`, tracking,
bulk `--query`/`--thread` on single-message actions, and `--raw-file` (a raw
message whose recipients the audit could not see). The four release executable hashes (macOS and Linux, arm64 and
amd64) live in `src/google/pin.json` with the version. latch compiles that file
in and takes both from it: the host looks for gog in that version's folder and
checks the hashes before each child starts.
