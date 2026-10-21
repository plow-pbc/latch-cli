# latch

A command-line peer that lets an agent reach the files, programs and Google
accounts on a machine its owner trusts, over Plow's directory.

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
