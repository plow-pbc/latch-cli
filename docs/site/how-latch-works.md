# How Latch works

A host does your agents' work. Its gatekeeper checks each request, and its
vault and audit log keep your secrets and a record of what happened.

## Hosts and agents

**A host** is a computer that runs `latch serve`. It does the work: it runs
programs, copies files and drives a browser. It also holds your vault and the
audit log.

**An agent** logs in with its own Plow credential. It can send requests to
your hosts. It can't change your device rule, read your vault's secrets or turn
connectors on.

**Plow is the directory.** It says who each caller is and where your hosts
are. A file's bytes and a program's output go straight between the two
computers, encrypted, and never through Plow. What the gatekeeper judges does
go to Plow: the request, the device rule and the message the agent cites,
with your later messages.

## The gatekeeper and the device rule

Before a host runs work that matters, its gatekeeper decides whether it may
run.

- **Reads run unjudged:** lookups in your calendar, mail, messages,
  contacts, files and the web, opening links and downloads, listing the
  vault, and Gmail drafts. An operation the host doesn't know is refused.
- **Everything else needs your message.** The agent passes `--intent
  ROOM/EVENT`, the id of your message it acts on, from its own 1:1 room
  with you (its Swarm DM with you, or its iMessage room). The host reads
  that message itself, with your later messages in its thread, and code
  refuses a missing citation, someone else's message or another room
  before any model is asked. Each host maps the agent to its Swarm account:
  `latch agents map my-agent @my-agent:plow.co`.
- **The judge asks two questions:** does your message, as your later
  messages leave it, cover the action, and does the device rule deny it.
  A later “stop” wins, and a ✅ or ❌ on the thread closes it. A one-time
  ask is spent once done. An agent quoting you, other people's messages
  and web pages are never permission.
- **A site task includes signing in there** with the login saved for it.
  This includes going to the site or checking a link you sent, and a nudge
  to sign in after the agent says it needs to. It gives no permission to
  buy, send, accept or change things afterwards. “Don't sign in” still
  wins. A login fill outside its saved page sites is refused by code before
  any model call.
- **Every refusal says what to do next,** such as how to cite.
- **It fails closed.** If the host can't judge a step, that step doesn't run.
  Earlier steps of a browser plan may already have acted.

**The device rule** is plain words, one per host: what agents are always
denied there (“Never touch my tax returns”). It grants nothing, and your
own calls are judged only against it. Setup asks “Always deny:” once. Edit
it on the host, or from another of your computers with `--host`:

```sh host
latch rules edit
latch rules edit --host my-mac
```

A model judges each action, so it isn't a guarantee.
`latch serve --deny-exec` is: no agent runs a program on that host.

## The vault

The vault keeps logins, cards, identities and notes on the host, encrypted.
Your agents use them without seeing them.

- You add secrets at a prompt, from your phone (`latch vault link`), or from
  another password manager's export (`latch vault import`).
- A fill puts a value into a web page. The agent never receives it.
- A saved login fills only on its own sites. A card waits in the page's
  fields, blank in screenshots, until the agent presses Pay itself, and an
  identity's values until the agent submits the form.
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
end. `latch dashboard` shows requests live. Each allowed action that needed
your message is recorded before it runs. Each gatekeeper call is also kept
for seven days, with its probabilities, prompt, model and cost. `latch audit
judge ID` reads one; Activity's “Gatekeeper requests” opens them.

## Where things run

| | Mac | Linux |
|---|---|---|
| Sandbox | Seatbelt | bubblewrap |
| The host runs as | a launchd job | a systemd user service |
| Secrets | the Keychain, in the release app | files only you can read |
| Messages and Contacts | Yes | No |
| Browser windows | on your screen | a private virtual display |
