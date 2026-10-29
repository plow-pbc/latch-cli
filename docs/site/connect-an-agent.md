# Connect an agent

Each agent logs in with its own Plow credential. Your hosts then judge each of
its requests as that agent's: against your rules, and what you told it in your
Plow chat.

> Never copy your `~/.latch` folder, or its token, to an agent. A host would
> take the agent's requests for yours.

## An agent on Plow

An agent that Plow runs already has its own credential. Run this where the
agent runs:

```sh
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh --client-only
~/.local/bin/latch login
```

There is no code to text: the login uses the agent's credential. You only
run it once.

## Your own agent

Make the agent from one of your own machines. It needs a free phone line:

```sh
latch agents lines
latch agents new --own --name my-agent --line LINE --token-file my-agent.token
```

`LINE` is a line's number from `latch agents lines`. The agent's credential
goes into the new file `my-agent.token`, which only you can read. Treat it
like a password.

Give the file to your agent. Its program logs in with it, in a state folder
of its own:

```sh
export PLOW_TOKEN_FILE=~/my-agent.token
export LATCH_STATE_DIR=~/latch-agent
latch login
```

Keep both set for every command the agent runs. To revoke the agent, run
`latch agents remove my-agent`.

## What an agent may do

Your agents can look things up in your calendar, email, messages and files,
and create or edit ordinary files, without asking first.

Riskier actions need your OK, in your rules or in your Plow chat with the
agent: sending, buying, deleting, changing calendars or shared systems,
giving a program internet access, using secrets, and sending data elsewhere.

Write your rules in your own words, for every agent or for one:

```sh
latch rules edit
latch rules edit --agent my-agent
```

To see what an agent would be allowed to do, without running it:

```sh
latch rules try --agent my-agent -- exec -- ls '~/code'
```

A model judges the rules, so they aren't a guarantee. For a hard stop, start
the host with `latch serve --deny-exec`: then no agent can run programs there.

## Built for agents

Every command prints one JSON object, and errors print as JSON too.

- `latch skill` prints the guide written for agents.
- `latch help COMMAND --examples` shows how to call a command.
- `--host NAME`, or `LATCH_HOST`, picks the host.

| Exit code | Means | Do |
|---|---|---|
| 0 | Done. | |
| 1 | Failed. | Read the error. |
| 2 | The command is wrong, or it names no host. | Fix the command. |
| 3 | The host couldn't be reached, or couldn't judge the request. | Retry later. A browser plan may have run some steps: read its `steps`. |
| 4 | Refused. | Don't retry. For `not_covered`, ask the owner in your Plow chat, then retry. |
| 5 | Sent, but no answer came back. | Check what happened before you send it again. |
