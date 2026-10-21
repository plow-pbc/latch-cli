# latch

A command-line peer that lets an agent reach the files, programs and browser
on a machine its owner trusts, over Plow's directory.

## Install

Teammates need access to this private repository and authenticated [GitHub CLI](https://cli.github.com/)
(`gh auth login`). Pick a release tag, then download and run its installer:

```sh
release=v0.1.0
d=$(mktemp -d) && gh release download "$release" -R plow-pbc/latch-cli -p install-release.sh -D "$d" && sh "$d/install-release.sh" "$release"
export PATH="$HOME/.local/bin:$PATH"
latch --version
```

The installer selects macOS Apple Silicon/Intel or Linux x86_64/arm64, verifies
its archive against the release's `SHA256SUMS`, and installs `~/.local/bin/latch`.
Pass `--bin-dir DIR` after the release tag to choose another binary directory.
The archive's executable `install.sh` installs the adjacent `latch` with no
arguments, accepts options before or after an explicit binary path, and shows
usage with `--help`.
It downloads pinned gog into `~/.latch/plugins`. Setup requires curl, tar and
SHA-256 tools.
Linux hosting needs bubblewrap and usable user namespaces; setup explains missing
packages and may ask for sudo, including for Ubuntu's AppArmor profile;
see [Docker hosting](latch/DOCKER.md).

Update by rerunning with a newer tag. State and versioned plugins stay in place;
restart an existing `latch serve` intentionally when ready to use the new binary.
macOS downloads have no Developer ID signature or notarization: use `gh` as above
rather than a browser download, which can add quarantine. No Homebrew tap or
Mac app is included.

## For agents

- **Log in with your own credential.** `latch login` with no flags finds a
  Plow agent's own credential (`$PLOW_AGENT_TOKEN`, the token file, or
  `$PLOW_API_BASE`'s proxy) and registers under the agent's name in Plow.
  Never use a device login copied from one of the owner's machines: a device
  login is the owner acting, and hosts never check it against what the owner
  asked for.
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

- `latch login` names a machine after its hostname; `--name` picks another.
  If another of your machines holds the name, the login stops and says so.
- `latch serve ~/work --deny-exec` refuses `exec` to every agent;
  `--deny-exec=AGENT_UID` (repeatable) to one agent, by its Plow uid. Your
  own machines are unaffected.
- A connection problem (`host_unreachable`, a slow call): set
  `LATCH_TRACE=iroh=debug,latch=info` on the client, the host or both, to
  see iroh's connection events (the filter syntax is `RUST_LOG`'s). They go
  to stderr, or to `serve.log` under the dashboard, never to stdout. On a
  host, each event names the connection's caller key and the operation.
- `latch shares set ~/work` saves the folders a `latch serve` named none
  shares. `latch status --local`, `latch events --follow` and
  `latch login --events` are for an app: see [latch/APP.md](latch/APP.md).
