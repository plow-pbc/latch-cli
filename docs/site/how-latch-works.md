# How Latch works

A host does your agents' work. Its gatekeeper checks each request, and its
vault and audit log keep your secrets and a record of what happened.

## Hosts, owners and agents

**A host** is a machine that runs `latch serve`. It does the work: it runs
programs, copies files and drives a browser. It also holds your vault and the
audit log.

**You are the owner.** Every machine you log in with your own Plow login acts
as you. From any of them you can manage the rules and the vault on any of your
hosts.

**An agent** logs in with its own Plow credential. It can send requests to
your hosts. It can't change your rules, read your vault's secrets or turn
connectors on.

**Plow is the directory.** It says who each caller is and where your hosts
are. A file's bytes and a program's output go straight between the two
machines, encrypted, and never through Plow. What the gatekeeper judges does
go to Plow: the request, your rules and your chat with the agent.

## The gatekeeper and rules

Before a host runs work that matters, its gatekeeper asks a model whether it
may run. It judges programs, file copies, Google, Messages, Contacts and the
browser's risky steps.

- **Your own requests** run unless your rules forbid them.
- **An agent's requests** are judged against your rules and what you told the
  agent in your Plow chat. Everyday lookups and ordinary file edits are
  allowed. Riskier actions need your OK.
- **Only you give permission.** What the agent says, other people's messages
  and web pages are never permission.
- **It fails closed.** If the host can't judge a step, that step doesn't run.
  Earlier steps of a browser plan may already have acted.

A model judges the rules, so they aren't a guarantee. `latch serve --deny-exec`
is: no agent runs a program on that host.

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

```sh
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
