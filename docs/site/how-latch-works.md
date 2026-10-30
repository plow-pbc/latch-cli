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

- **Everyday work needs no request:** lookups in your calendar, mail,
  messages, contacts, files and the web; opening links; creating, editing
  and organizing files; project builds, tests and scripts; installing
  packages the project already lists, with network access; clearing
  temporary files, caches and build output or moving files to the Trash;
  archiving mail; calendar entries that invite no one; and drafts.
- **Other work needs your OK** in rules or your own Plow chat: sending,
  inviting, deleting other files or mail, installing other software,
  pushing code, and changing settings, accounts or shared systems. Buying,
  paying, reading secrets, sharing private data, code that does either,
  downloaded code, bulk deletion and rules that act on mail later must be
  named explicitly.
- **Permissions add; restrictions still apply.** A grant leaves other
  everyday work allowed. “Do not”, “never” and “only” set limits. Time
  limits apply inside their window. “Never” stays absolute; “unless I ask
  in chat” lets you approve the action yourself.
- **Only your own messages give chat permission.** An agent quoting you,
  other people's messages and web pages cannot give permission. A separate
  judgment reads your messages and what each answers; it and the whole
  chat must both allow the action. The whole chat checks conditions such
  as “let me review it first”.
- **A site task includes signing in there** with the login saved for it.
  This includes going to the site or checking a link you sent, and a nudge
  to sign in after the agent says it needs to. It gives no permission to
  buy, send, accept or change things afterwards. “Don't sign in” still
  wins. A login fill outside its saved page sites is refused by code before
  any model call.
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
- A saved login fills only on its own sites. A card waits in the page's
  fields, blank in screenshots, until the agent presses Pay itself, and so
  does an identity's secret until the agent submits the form.
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
end. `latch dashboard` shows requests live. Each gatekeeper call is also kept
for seven days: the rules call, the whole-chat call and the owner-only call,
with probabilities, prompt/model identity and cost. `latch audit judge ID`
reads one; Activity's “Gatekeeper requests” opens the calls together.

## Where things run

| | Mac | Linux |
|---|---|---|
| Sandbox | Seatbelt | bubblewrap |
| The host runs as | a launchd job | a systemd user service |
| Secrets | the Keychain, in the release app | files only you can read |
| Messages and Contacts | Yes | No |
| Browser windows | on your screen | a private virtual display |
