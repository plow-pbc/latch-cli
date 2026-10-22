# latch

A command-line peer that lets an agent reach the files, programs and browser
on a machine its owner trusts, over Plow's directory.

## Install

Teammates need access to this private repository and authenticated [GitHub CLI](https://cli.github.com/)
(`gh auth login`). Releases are tagged `v<version>-<build>`, e.g. `v0.1.0-1327`, where the build
number counts release runs; `latch --version` shows it as `0.1.0 (1327)`. This installs the newest published one; set
`release` to a tag to pick another:

```sh
release=$(gh release view -R plow-pbc/latch-cli --json tagName --jq .tagName)
d=$(mktemp -d) && gh release download "$release" -R plow-pbc/latch-cli -p install-release.sh -D "$d" && sh "$d/install-release.sh" "$release"
export PATH="$HOME/.local/bin:$PATH"
latch --version
```

The installer selects macOS Apple Silicon/Intel or Linux x86_64/arm64, verifies
its archive against the release's `SHA256SUMS`, and installs `~/.local/bin/latch`.
On macOS that is a link into `~/Applications/Plow Latch.app`, the signed app
latch runs in (no window or Dock icon), which Full Disk Access is granted to.
Pass `--bin-dir DIR` after the release tag to choose another binary directory.
The archive's executable `install.sh` installs the adjacent `latch` with no
arguments, accepts options before or after an explicit binary path, and shows
usage with `--help`.
It downloads pinned gog into `~/.latch/plugins`. Setup requires curl, tar and
SHA-256 tools.
Linux hosting needs bubblewrap and usable user namespaces; setup explains missing
packages and may ask for sudo, including for Ubuntu's AppArmor profile;
see [Docker hosting](latch/DOCKER.md).

Update by rerunning the commands above; `install-release.sh latest` also
resolves the newest published release itself. State and versioned plugins stay in place;
restart an existing `latch serve` intentionally when ready to use the new binary.
The macOS binaries are signed with Plow's Developer ID and notarized by Apple.
No Homebrew tap is included.

## For agents

- **Log in with your own credential.** `latch login` with no flags finds a
  Plow agent's own credential (`$PLOW_AGENT_TOKEN`, the token file, or
  `$PLOW_API_BASE`'s proxy) and registers under the agent's name in Plow.
  Never use a device login copied from one of the owner's machines: a device
  login is the owner acting, and hosts judge it as the owner's own request,
  not against what the owner asked the agent for.
- **Denied, ask, retry.** A host's gatekeeper lets an agent do what the owner
  asked for, in their Plow chat with that agent or in the host's rules. A
  refusal is exit 4, `gatekeeper_denied`:
  - `not_covered`: ask the owner in your Plow chat, naming exactly what you
    will do, then retry once they agree. This is the one exit 4 to retry.
  - `owner_said_no`: don't retry.
  - Exit 3, `gatekeeper_unavailable`: the host could not judge; retry later.
- **`exec_denied`** (exit 4): the host's owner turned `exec` off for agents
  there (`latch serve --deny-exec`). Asking does not change it.

## For hosts

- `latch serve ~/work` starts this machine's host as a service of yours (a
  launchd agent on macOS, running the `latch` in `Plow Latch.app`; a systemd
  user service on Linux) and shows its dashboard: Ctrl-C there, or closing
  the terminal, stops the host. `latch serve -d` starts it detached and
  prints it as JSON once it answers; `latch stop` stops it, and nothing
  starts it again until the next `latch serve`. `latch dashboard` views a
  running host (Ctrl-C closes only the view), `latch logs [-f]` shows its
  log (`serve.log`) and `latch audit show [-f]` its audit.
  One host runs per state folder. `latch serve --foreground` runs the host
  in the terminal's own process, logging to stderr, for CI, tests and
  containers.
- `latch login` names a machine after its hostname; `--name` picks another.
  If another of your machines holds the name, the login stops and says so.
- `latch serve ~/work` lets callers' programs and file copies read and
  write `~/work`, and read the system's own files (programs, libraries,
  developer tools, Homebrew, certificates); nothing else of the machine.
  `--allow-read PATH` (repeatable) adds a file or folder they may read but
  never write, e.g. `--allow-read ~/.rustup --allow-read ~/.cargo` for a
  Rust toolchain kept in the home folder.
- Credentials and private data are never readable, even inside a shared or
  `--allow-read` folder: `~/.ssh`, `~/.gnupg`, keychains, browser profiles,
  Mail, Messages, Calendars, Contacts, macOS's privacy settings (TCC),
  latch's state (any state folder in the shares too), `~/.aws`,
  `~/.config/gcloud`, `~/.azure`, `~/.kube`, `~/.docker/config.json`,
  `~/.netrc`, `~/.npmrc`, `~/.git-credentials` and `~/.config/gh`. An
  `--allow-read` inside one of them is refused.
- The home folder, its `Library` and the folders above them (`/Users`, `/`)
  can be neither shared nor named with `--allow-read`: name the projects in
  it instead. `latch serve --help` has the whole list of what callers can
  reach.
- `latch serve ~/work --deny-exec` refuses `exec` to every agent;
  `--deny-exec=AGENT_UID` (repeatable) to one agent, by its Plow uid. Your
  own machines are unaffected.
- A connection problem (`host_unreachable`, a slow call): set
  `LATCH_TRACE=iroh=debug,latch=info` on the client, the host or both, to
  see iroh's connection events (the filter syntax is `RUST_LOG`'s). They go
  to the host's log (`latch logs`), never to stdout. On a
  host, each event names the connection's caller key and the operation.
- The host's audit log is sealed: each record is encrypted with the host's
  audit key, kept with its other secrets. `latch audit show` prints every
  record in full, each command as it ran; `latch audit verify` checks the
  chain without the key; `latch audit rotate-key` starts a new key and keeps
  the old ones (a running `latch serve` uses it once restarted). The serve
  dashboard and `serve.log` hide credential-looking values in commands, best
  effort: some can slip through. The sealed log is
  `audit.sealed.ndjson`; a plaintext `audit.ndjson` from an older latch is
  never read, and latch says it can be deleted.
- `latch shares set ~/work [--allow-read PATH]` saves the folders, and the
  read-only paths, a `latch serve` named none shares. With none named and
  none saved, it serves the browser and Google only, and `exec` and `file`
  refuse with `no_folders`. `latch status --local`,
  `latch events --follow` and `latch login --events` are for an app: see
  [latch/APP.md](latch/APP.md).

## On the host itself

After `latch serve` has registered this machine, commands with no `--host`
use its private local receiver. `--host NAME` or `LATCH_HOST` still selects
another host; naming this machine uses the same local receiver. Exec,
files, browser and Google calls pass through the same gatekeeper and audit as remote
calls, recorded as `owner_on_host`. If the receiver is stopped, start
`latch serve` on the same state folder (`serve_required`, exit 3).

Local `vault`, `rules` and `shares` configuration works without `serve` or
the decision model. Vault edits and rules apply to the running receiver's
next operation; changes to shared folders apply when it restarts.
