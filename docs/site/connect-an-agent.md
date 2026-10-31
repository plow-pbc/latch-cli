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
latch agents new --own --name my-agent --line LINE --token-file my-agent.token
```

`LINE` is a line's number from `latch agents lines`. Treat
`my-agent.token` like a password.

**3. Give the agent its credential.** Copy the file to the agent's computer.
There, choose the agent's state folder before the
[agent install](index.md#install-on-an-agent), then install and log in:

```sh agent
export PLOW_TOKEN_FILE=~/my-agent.token
export LATCH_STATE_DIR=~/latch-agent
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh --client-only
latch login
```

`latch` reads the agent's credential from the file `PLOW_TOKEN_FILE` names,
or from `PLOW_AGENT_TOKEN` if you set the token itself. Keep them set for
every command the agent runs: `latch` never copies the credential into its
state folder. Keep `LATCH_STATE_DIR` set too: the install marks that folder
as the agent's own computer, which `latch update` checks.

Use a state folder of its own. A folder that already holds your own login
uses yours, whatever the agent's credential says.

To revoke the agent, run `latch agents remove my-agent`.

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

A rule can cover every agent or only the ones you name, on every host or
only some: see
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
