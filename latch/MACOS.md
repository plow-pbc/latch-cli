# latch on macOS

On macOS a release's latch is `~/Applications/Plow Latch.app`. The app is
not a separate program: its executable, `Contents/MacOS/latch`, is the same
`latch` binary Linux ships bare. The bundle exists to give that binary a
signed identity, so the owner can grant it Full Disk Access the supported
way and it can keep its secrets in the Keychain.

## The bundle

`scripts/macos-app-bundle.sh` builds it from a latch binary:

- `Contents/MacOS/latch`, the bundle's executable (`CFBundleExecutable`);
- `Contents/Info.plist`, written by the script: bundle id
  `co.plow.latch.cli`, `LSUIElement` (no window, no Dock icon), macOS 13 or
  later. `CFBundleShortVersionString` is the version (0.1.0) and
  `CFBundleVersion` a release's build number (1327 for `v0.1.0-1327`), so
  each build is distinct; a version with no build number is used for both;
- `Contents/Resources/AppIcon.icns` (`latch/macos/`);
- `Contents/embedded.provisionprofile`, the Developer ID profile that lets
  the signature claim latch's keychain group.

Opened from the Finder, the app does nothing visible: `latch` with no
arguments and no terminal is a usage error.

`scripts/package-release.sh` puts the bundle in a macOS release archive,
ad hoc signed, with a `latch` symlink beside `install-binary.sh` that leads
into it. The release workflow (`.github/workflows/release.yml`) then signs
the whole bundle with Plow's Developer ID, the hardened runtime and
`latch/macos/latch.entitlements` (app id `3559PD337Z.co.plow.latch.cli`,
keychain group `3559PD337Z.co.plow.latch`), notarizes it and staples the
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
one there, and makes `~/.local/bin/latch` (or `--bin-dir`'s) a symlink to
`~/Applications/Plow Latch.app/Contents/MacOS/latch`. The link's target
never changes, so later installs leave it alone.

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
build` and the tests. Such a latch keeps its secrets as files, and Full Disk
Access is granted to the binary itself. `install-binary.sh` given a bare
binary on macOS copies it into the bin folder as it does on Linux; `just
cft` and the macOS CI tests install that way. CI builds the bundle in
`mac-tests.yml` (ad hoc signed, to check it is valid) and `keychain.yml`
(Developer ID signed, to test the Keychain).
