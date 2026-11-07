# Connect an agent

Every agent needs its own Plow credential. Your hosts recognize the agent by
it, and judge each of its requests as that agent's: against your rules, and
what you told it in your Plow chat.

> Never copy your own `~/.latch` folder, or its token, to an agent. A host
> would take the agent's requests for yours.

## An agent Plow runs

**Agents built on Plow's [OpenClaw](https://github.com/plow-pbc/plow-openclaw-agent)
or [Hermes](https://github.com/plow-pbc/plow-hermes-agent) images:** nothing to
set up. `latch` comes installed and signed in.

**Any other agent on Plow:** its credential already works with `latch`. Give
it the [Agent install](index.md#install-on-an-agent), then `latch login`.

## Your own agent

An agent you run yourself, anywhere: your laptop, a server or a container.
It gets its credential in three steps.

**1. Pick a phone line.** Each agent answers on one of your Plow phone lines.
That's where you chat with it. From one of your own computers:

```sh host
latch agents lines
```

**2. Register the agent.** This tells Plow about an agent you run yourself
(that's what `--own` means), on that line, and writes its credential to a new
file that only you can read:

```sh host
latch agents new --own --name my-agent --line LINE --credential-file my-agent.env
```

`LINE` is a line's number from `latch agents lines`. `my-agent.env` is a new
file, in a folder that already exists. Latch makes it private (mode 600) and
never overwrites a file. Treat it like a password: it sets `PLOW_API_BASE`
and `PLOW_AGENT_TOKEN`, which the agent's program loads to log in.

**3. Give the agent its credential.** Copy the file to the agent's home
folder. There, load it and choose the agent's state folder before the
[agent install](index.md#install-on-an-agent), then install and log in:

```sh agent
set -a
. ~/my-agent.env
set +a
export LATCH_STATE_DIR=~/latch-agent
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh --client-only
latch login
```

Keep both variables set for every command the agent runs. Latch never copies
the credential into its state folder. Keep `LATCH_STATE_DIR` set too: the
install marks that folder as the agent's own computer, which `latch update`
checks.

Use a state folder of its own. A folder that already holds your own login
uses yours, whatever the agent's credential says.

To revoke the agent, run `latch agents remove my-agent`.

## Moving from plow-agents

Use Latch to create, list and remove agents. Keep using `plow-agents`, with
its own login, for profiles and image publishing. Latch's agent commands
print JSON, and every new agent needs a `--name`. Removing an agent asks
first; `--yes` skips the question.

`LINE` is a phone line's uid or number from `latch agents lines`. `REF` is
your public image's full `REGISTRY/PATH@sha256:DIGEST` reference, with 64
lowercase hex characters in `DIGEST`.

| plow-agents | Latch |
|---|---|
| `login` | `latch login`, or `latch login --events` without a terminal |
| `--api-base URL` | `latch login --api-url URL` |
| `--token-file FILE`, for another account | `latch --state-dir DIR login`, then use `--state-dir DIR` on each command |
| `lines` | `latch agents lines` |
| `agents` | `latch agents`; for one agent, `latch agents show NAME` |
| `mint LINE` | `latch agents new --own --name NAME --line LINE --credential-file ./plow-credentials` |
| `mint LINE --agent-api-base URL` | Add `--agent-api-url URL` to that command. |
| `deploy exe:SLUG --line LINE` | `latch agents new --cloud exe:SLUG --name NAME --line LINE` |
| `deploy REF --line LINE` | `latch agents new --image REF --name NAME --line LINE` |
| `deploy --local --line LINE` | The two steps below. |
| `revoke LINE` | `latch agents remove --line LINE` |
| `revoke` using the file's uid | `latch agents remove UID`, using the creation result or the file's `# plow-agent-uid:` comment. |
| `rotate` | No replacement. |
| `profile`, `image build/push/show/set/promote` | Keep using `plow-agents` with its publishing login. |

### Running with Docker Compose

Use an existing Compose project whose service reads
`env_file: ./plow-credentials`. List `plow-credentials` in `.gitignore` and
`.dockerignore` before building. Latch doesn't check either file. The
destination folder must already exist.

```sh host
latch agents new --own --name my-agent --line LINE --credential-file ./plow-credentials
docker compose up --build -d
```

If Compose fails, fix that failure and run Compose again: the agent already
exists. If the container reaches Plow at another address, add
`--agent-api-url URL` to the first command, such as
`--agent-api-url http://host.docker.internal:18934` for a local stack.

Removing the agent revokes its credential but doesn't delete the local file.
After successful removal, delete the file explicitly when you no longer need
it.

## What an agent may do

By default, agents may look things up, open web pages, work on your files,
run project builds and tests, and install packages the project already
lists. Sending, inviting, deleting other files and changing accounts or
shared systems need your permission. Buying, reading secrets or sharing
private data must be named explicitly in your rules or your own Plow chat.
A task you give on a site includes using its saved login there; it does not
extend to buying, sending or changing things after sign-in.

A permission adds to these defaults. A rule that grants one task leaves
other everyday work allowed; a restriction still holds. “Never” stays
absolute. “Unless I ask in chat” lets you approve the action yourself, in
your own messages. An agent quoting you cannot give permission.

For example:

- “Do not read or search this Mac's Messages.”
- “Agents may email the people in my contacts.”
- “Never buy anything unless I ask in chat.”

Each host keeps its own rules: see
[How Latch works](how-latch-works.md#the-gatekeeper-and-rules).

## Built for agents

Every command prints one JSON object, and errors print as JSON too.

- `latch skill` prints the guide written for agents.
- `latch help COMMAND --examples` shows how to call a command.
- `--host NAME`, or `LATCH_HOST`, picks the host. See
  [Choosing a host](commands.md#choosing-a-host).

```sh agent
latch exec --host my-mac -- ls -la
```

| Exit code | Means | Do |
|---|---|---|
| 0 | Done. | |
| 1 | Failed. | Read the error. |
| 2 | The command is wrong, or it names no host. | Fix the command. |
| 3 | The host couldn't be reached, or couldn't judge the request. | Retry later. A browser plan may have run some steps: read its `steps`. |
| 4 | Refused. | Don't retry. For `not_covered`, ask the owner in your Plow chat, then retry. |
| 5 | Sent, but no answer came back. | Check what happened before you send it again. |
