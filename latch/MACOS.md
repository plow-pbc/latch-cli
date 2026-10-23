# latch on macOS

On macOS a release's latch is `~/Applications/Plow Latch.app`. Its
executable, `Contents/MacOS/latch`, is the same `latch` binary Linux ships
bare. The bundle gives that binary a signed identity, so the owner can grant
it Full Disk Access the supported way and it can keep its secrets in the
Keychain, and it carries the app's window.

## The bundle

`scripts/macos-app-bundle.sh` builds it from a latch binary:

- `Contents/MacOS/latch`, the bundle's executable (`CFBundleExecutable`);
- `Contents/Frameworks/libPlowLatchUI.dylib`, the app's window: a SwiftUI
  library (`latch/gui/`, built universal by `gui/build.sh`, Xcode 26 or
  later);
- `Contents/Info.plist`, written by the script: bundle id
  `co.plow.latch.cli`, macOS 13 or later. `CFBundleShortVersionString` is
  the version (0.1.0) and `CFBundleVersion` a release's build number (1327
  for `v0.1.0-1327`), so each build is distinct; a version with no build
  number is used for both;
- `Contents/Library/LaunchAgents/co.plow.latch.cli.login.plist`, the
  agent behind the window's "Start when you log in": registered with
  ServiceManagement, it runs the bundle's `latch serve -d --again` at login,
  on latch's default state folder, so the window offers it only when it
  runs on that folder;
- `Contents/Resources/AppIcon.icns` (`latch/macos/`);
- `Contents/embedded.provisionprofile`, the Developer ID profile that lets
  the signature claim latch's keychain group.

**The window.** When LaunchServices opens the app (the Finder, the Dock,
`open`, a login item) with no arguments, or exactly `--state-dir PATH`
(`open APP --args --state-dir PATH`), latch sees its own bundle id in
`__CFBundleIdentifier`, loads the window's library and hands it the main
thread (`src/gui.rs`): a normal Dock app. The window needs macOS 26 or
later; below that the app shows one alert pointing to `latch setup` and
quits. Every other run of the same binary is the CLI and loads no AppKit or
SwiftUI. The window gets everything it shows from this bundle's own latch,
run with `--state-dir`. The window remembers a `--state-dir` it was opened
with and goes back to it on a launch with no arguments, since macOS's Quit &
Reopen (after Full Disk Access is granted) drops them; opening it with
`--state-dir` naming latch's default folder forgets it.

The window walks a new owner through setup (log in, rules, Google,
Contacts & Messages, starting latch), each step a latch command
(`login --events`, `rules show/set`, `rules try`, `plugins enable`,
`serve -d`), then shows this Mac's `status --local`. It remembers only which
screens were passed, in its own preferences; a relaunch resumes at the first
screen not passed. `swift run Snapshots DIR` in `latch/gui` renders every
screen and state, light and dark, on a sample latch; `gui/build.sh
--test-hooks` builds a window an end-to-end run can drive.

`gui/hooks.sh` drives that build from a terminal, for a local check of
launch, the flow's state and what it records (`App/TestHooks.swift`; not
run in CI). Its commands are model intents: `send primary` calls the
flow's primary action as its button would, but no control, focus or
keyboard is involved, so it is not a click test. It builds its own app,
`.dev/hooks/Plow Latch Hooks.app` (bundle id `co.plow.latch.hooks`), on
this checkout's debug latch with no Plow, and opens it on a fresh
`.dev/tests/hooks/state`. That app is background-only: no window, no Dock
icon, and its flow starts without a window, so a screen's own work (Log in
starting a login as it appears) doesn't run. `build --window` makes one
that shows its window, for the hooks that snapshot or resize it. `quit`
deletes the app's preferences. For example, Welcome's primary action
leading to Log in:

```sh
latch/gui/hooks.sh build    # about 1 min cold, 20 s warm
latch/gui/hooks.sh open     # "opened on …/.dev/tests/hooks/state"
latch/gui/hooks.sh state    # "step": "welcome", "windows": []
latch/gui/hooks.sh send primary
latch/gui/hooks.sh state    # "step": "login", "passed": {"welcome": "continued"}
latch/gui/hooks.sh quit
```

From `open` to `quit` this took 1.6–2.7 s on an M5 Max.

`open --fixture SPEC` opens it on made-up data instead of latch's
(`App/Synthetic.swift`): logged in, set up, this Mac serving, and the main
window showing (with `build --window`). SPEC sizes it, for measuring the
main window: `hosts=` 1–8 (this Mac, one online, one offline, then a long
name and one whose state Plow can't say), `records=` on each host,
`vault=` items on each, `agents=` 1–8, `rate=` new records a second, and
`delay=` milliseconds each read takes. `send measure PATH` then times every
hook after it, one JSON line each in PATH, with the hook's name but never
its argument (a vault value can be one): `visible_ms`, until the window
server's image of the window first changes, looked at on each display
refresh for up to 2 s (null if it didn't change), and `next_refresh_ms`,
when the refresh after the action was due, which only schedules a frame
and is no proof the change was on it. Both are null with no window. `send
measure` with no path stops. Each timed hook and each screen's reads are
signposted (`co.plow.latch`, Points of Interest) for Instruments, the
evidence for a visible-response budget alongside `visible_ms`.

```sh
latch/gui/hooks.sh build --window
latch/gui/hooks.sh open --fixture hosts=8,records=10000,vault=1000,agents=5
latch/gui/hooks.sh send measure "$PWD/.dev/tests/timings.jsonl"
latch/gui/hooks.sh send host pi-kitchen
latch/gui/hooks.sh send section activity   # {"command":"section","next_refresh_ms":…,"visible_ms":…}
latch/gui/hooks.sh quit
```

`scripts/package-release.sh` puts the bundle in a macOS release archive,
ad hoc signed (`scripts/macos-sign-app.sh`), with a `latch` symlink beside
`install-binary.sh` that leads into it. The release workflow
(`.github/workflows/release.yml`, on macOS 26's Xcode) then signs it inside
out with the same script: the window's library, then the bundle, with
Plow's Developer ID, the hardened runtime and
`latch/macos/latch.entitlements` (app id `3559PD337Z.co.plow.latch.cli`,
keychain group `3559PD337Z.co.plow.latch`); it notarizes it and staples the
ticket.

## What the identity is for

- **Full Disk Access** (Messages, Contacts) is granted to the app. macOS
  holds a grant against the bundle id and its signer, so a new build signed
  the same way keeps it. `latch setup` and `latch plugins enable` name the
  app to grant: the `.app` the running latch is inside, or the binary
  itself when it is in none (`src/plugins/host.rs`).
- **launchd runs the app's latch.** `latch serve` starts the host as a
  launchd job running the canonical path of the latch it was run as, which
  through the link is the one inside the app, so macOS treats the host as
  the app and not as the terminal that started it (`src/managed.rs`). The
  Full Disk Access check with no host running is a one-shot launchd job for
  the same reason (`src/messages/access.rs`). The jobs' plists are in the
  state folder, loaded for that start only, never at login.
- **The Keychain.** A latch whose signature holds latch's keychain group
  keeps its key, Plow token and vault custody key there; any other build
  keeps them as files in the state folder (`src/secrets.rs`).

## Install and update

`install-binary.sh` sees that the `latch` it was given leads into an app
bundle. Then it copies the whole bundle to `~/Applications`, replacing the
one there unless every file is the same (a new window with the same latch
is a new app), and makes `~/.local/bin/latch` (or `--bin-dir`'s) a symlink to
`~/Applications/Plow Latch.app/Contents/MacOS/latch`. The link's target
never changes, so later installs leave it alone.

A first install at a terminal (no login in the state folder yet, no
`--non-interactive`) ends by opening the installed app on macOS 26 or
later, with `open APP --args --state-dir PATH` when `--state-dir` was
given (as an absolute path), so its window takes the owner through setup;
before macOS 26 it runs `latch setup` in the terminal, as on Linux. Before
macOS 13 the installer refuses and installs nothing.

`latch update` (`src/update.rs`) downloads the release archive, checks it
against the signed `SHA256SUMS`, and refuses the app unless it is
`co.plow.latch.cli` signed by team 3559PD337Z, intact, and accepted by
Gatekeeper. Then it runs that release's own `install-binary.sh`, with no
`--bin-dir` on macOS: the app in `~/Applications` is what gets replaced, so
a link to it anywhere keeps working, and the link the installer checks is
`~/.local/bin/latch`. The installer:

1. stops a host running the latch being replaced (a different version),
   with the new latch;
2. copies the new app beside the old one, moves the old one aside, moves
   the new one into its place, then removes the old one. If a move fails
   the old app goes back, still working;
3. links `~/.local/bin/latch` into it, if the link is not there already;
4. installs what lives outside the bundle (below);
5. starts a host that `serve -d` had started again, with what it was
   given. A host that ran in a terminal stays stopped, and the update says
   to run `latch serve` there again.

So on macOS the app is replaced as a unit and the binary is never copied
anywhere else. Every install and update also writes these outside the
bundle, into the state folder (`~/.latch`, or `--state-dir`):

- gog, `plugins/gog/<version>/`;
- the Messages reader, `plugins/messages/<latch version>/latch-messages`: a
  separate Swift binary (`latch/messages-reader`) that latch runs only if
  its sha256 is the one latch was built with. The release workflow signs
  the reader with Plow's Developer ID and notarizes it with the app;
- the browser engines, `plugins/cft/` and `plugins/camoufox/`.

## Dev and CI builds

`just build` makes a bare `.dev/bin/latch`, not an app, and so do `cargo
build` and the tests. `just app` wraps that latch, with the window, in
`.dev/app/Plow Latch Dev.app`, under its own bundle id
(`co.plow.latch.dev`), so it never shares the installed app's grants,
preferences or windows, and installs gog, the Messages reader and Chrome
for Testing into its state folder (`.dev/state`, or `.dev/state-prod` for a build on another
Plow than the local stack); `open` it to see the window. Such a latch keeps its secrets as files, and Full Disk
Access is granted to the binary itself. `install-binary.sh` given a bare
binary on macOS copies it into the bin folder as it does on Linux; the
macOS gate's browser tests install that way. The bundle, with the window
built for Apple silicon and Intel (Xcode 26 or later), is built by `just
test`'s installer suite on macOS (`scripts/test-app-bundle.sh`: ad hoc
signed, to check it is valid; with the window's Swift tests), which
`mac-tests.yml` runs on macOS 26, and by `keychain.yml` (Developer ID
signed, to test the Keychain). Every pull request also builds the macOS
archives on macOS 26 and runs them on the oldest macOS images GitHub offers
(macOS 15, Apple silicon and Intel): `scripts/macos-smoke.sh` runs latch's
offline commands, the Messages reader up to a database it may not read,
and opens the app, which must show its needs-macOS-26 alert.
