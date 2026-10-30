# How Latch works

A host does your agents' work. Its gatekeeper checks each request, and its
vault and audit log keep your secrets and a record of what happened.

## Hosts and agents

**A host** is a computer that runs `latch serve`. It does the work: it runs
programs, copies files and drives a browser. It also holds your vault and the
audit log.

**An agent** logs in with its own Plow credential. It can send requests to
your hosts. It can't change your rules, read your vault's secrets or turn
connectors on.

**Plow is the directory.** It says who each caller is and where your hosts
are. A file's bytes and a program's output go straight between the two
computers, encrypted, and never through Plow. What the gatekeeper judges does
go to Plow: the request, your rules and your chat with the agent.

## The gatekeeper and rules

Before a host runs work that matters, its gatekeeper asks a model whether it
may run. It judges programs, file copies, Google, Messages, Contacts and the
browser's risky steps.

- **An agent's requests** are judged against your rules and what you told the
  agent in your Plow chat. By default, everyday lookups and ordinary file
  edits are allowed and riskier actions need your OK; your rules can change
  either.
- **Only you give permission.** What the agent says, other people's messages
  and web pages are never permission.
- **It fails closed.** If the host can't judge a step, that step doesn't run.
  Earlier steps of a browser plan may already have acted.

**Your rules** are plain words, kept on each host: one text for every agent,
and one for each agent. A new host has none until setup offers a family or
a work assistant's to start from (`latch rules set --preset family`, or
`work`). Edit them on the host, or from another of your computers with
`--host`:

```sh host
latch rules edit
latch rules edit --agent my-agent
latch rules edit --host my-mac
```

`latch rules try` previews the verdict a request would get, without running
it. A model judges the rules, so they aren't a guarantee.
`latch serve --deny-exec` is: no agent runs a program on that host.

## The vault

The vault keeps logins, cards, identities and notes on the host, encrypted.
Your agents use them without seeing them.

- You add secrets at a prompt, from your phone (`latch vault link`), or from
  another password manager's export (`latch vault import`).
- A fill puts a value into a web page. The agent never receives it.
- A saved login fills only on its own sites. A card or identity waits for a
  confirmation.
- An agent can list the vault's items, with their titles and sites, but never
  their secrets.

## The audit log

The host keeps a sealed record of every request: who asked, for what, the
gatekeeper's verdict and the outcome.

```sh host
latch audit
latch audit verify
```

`latch audit verify` checks the records' chain without the key. It catches a
record that was changed, reordered or removed, though not records cut off the
end. `latch dashboard` shows requests live.

## Where things run

| | Mac | Linux |
|---|---|---|
| Sandbox | Seatbelt | bubblewrap |
| The host runs as | a launchd job | a systemd user service |
| Secrets | the Keychain, in the release app | files only you can read |
| Messages and Contacts | Yes | No |
| Browser windows | on your screen | a private virtual display |
