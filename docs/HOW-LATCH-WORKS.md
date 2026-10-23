# How latch works

## 1. In short

latch lets your agents use the files, programs and browser of a machine
you trust, your Google account through it, and on a Mac its Messages and
Contacts. That machine runs `latch serve` and is called a **host**; your
other machines and your agents call it over an encrypted connection between
the two machines, with Plow as the directory that says who each caller is.
The host's gatekeeper checks each action against what you asked for, and
the host records every call in its audit log.

| | Who it is | What it can do | How its calls are judged |
|---|---|---|---|
| **Owner** | Your machines, logged in with your own Plow login. | Call any host, manage the rules and vault on any host, turn connectors on or off on the host itself, run a host. | As your own request: refused only where the host's rules forbid it. |
| **Owner on the host** | Anything running as your OS user on the host that uses its state folder. An agent running there counts. | Everything the owner can, on that host, through its local socket. | Like the owner. |
| **Agent** | An agent logged in with its own Plow agent credential. | Call hosts. It can't manage rules, the vault or connectors, and can't run a host. | Against your messages in its Plow chat, plus the host's rules. |
| **Host** | A machine running `latch serve`. | Runs programs, file copies and connector calls in a sandbox, and holds the vault, rules, browser and audit log. | Its gatekeeper judges callers' actions; an action it can't judge doesn't run. |

**Read more:** [who's who in detail](#2-whos-who) ·
[Mac and Linux hosts](#3-hosts-mac-and-linux) ·
[what an agent can do](#4-what-an-agent-can-do-through-a-host) ·
[the gatekeeper](#5-the-gatekeeper) ·
[managing a host](#6-managing-a-host) · [trust](#7-trust-model) ·
[install and update](#8-install-update-log-out) ·
[not supported](#9-not-supported) ·
[direction](#10-direction-not-built-yet). For the detail, see
[Getting started](GETTING-STARTED.md) and the guides for the
[browser](../latch/BROWSER.md), [vault](../latch/VAULT.md),
[Google](../latch/GOOGLE.md), [macOS](../latch/MACOS.md) and
[Docker](../latch/DOCKER.md).

Everything here describes `main` today. Lines marked **Next build** are in
pull requests that haven't merged yet.

## 2. Who's who

| | What it is | Calls hosts | Rules, vault, connectors | Runs a host | How its calls are judged |
|---|---|---|---|---|---|
| **Owner** (your CLI) | A machine logged in with your own Plow login: by text, or with `--token-file`. | Yes, any host in your account. | Rules and vault on any host (`--host NAME`). Connectors only on the host itself. | Yes. | As your own request: refused only where the host's rules forbid it. |
| **Owner on the host** | Anything running as the host's OS user that uses the host's own state folder, with no `--host` (or `--host` naming this machine). An agent running there counts. | That host, through its private local socket; other hosts as the owner. | Yes, on that host. Rules and vault work without `latch serve` or the decision model. | It is the host. | Exactly like the owner; recorded as `owner_on_host`. |
| **Agent** | An agent logged in with its own Plow agent credential, in its own state folder. | Yes. | No: rules, vault and turning connectors on or off are refused (`owner_only`, exit 4). It may list connectors. | No: `latch serve` stops with `not_logged_in`. | Against your messages in your Plow chat with that agent, plus the host's rules. |

- **Every machine logged in with your own Plow login is the owner today.**
  There's no host-only or view-only login yet (see section 10).
- **A host knows a caller only by its key.** It asks Plow who that key is
  when the caller connects and again before every operation, and refuses
  the call if Plow says no or can't be reached.
- **Never copy a machine's state folder or Plow token to where an agent
  runs.** The host would take the agent for you, and judge its calls only
  against your rules.

## 3. Hosts: Mac and Linux

| | Mac | Linux, including a Raspberry Pi |
|---|---|---|
| Supported | Apple silicon and Intel | x86_64 and arm64 |
| Sandbox for exec, files, Google and Messages | Seatbelt | bubblewrap and seccomp; needs bubblewrap and user namespaces (the installer may ask for sudo) |
| `latch serve` runs as | a launchd job, using the `latch` inside `~/Applications/Plow Latch.app` | a systemd user service |
| Key, Plow login, vault and audit keys | Keychain, in the signed release app | files only you can read (0600) |
| Messages and Contacts | Yes, once turned on and given Full Disk Access (granted to Plow Latch.app) | No |
| Chrome for Testing | Yes | arm64 only; there's no x86_64 build |
| Camoufox | Yes | Yes |
| The browser's sandbox | Chrome's own; the engine runs outside latch's Seatbelt | inside latch's bubblewrap, with Chrome's own sandbox nested inside |
| Browser windows | on your screen; `browser handoff` works | on a private virtual display; `browser handoff` answers `needs_owner` (`no_screen`) |

- A host that can't set up its sandbox refuses to start. There's no
  unsandboxed mode.
- On Ubuntu hosts where AppArmor restricts user namespaces, Chrome runs
  without its own sandbox, inside latch's, and `latch status --local` says
  `page isolation off on this host`.
- `latch serve --headless-browsers` runs every browser with no window, so
  handoff isn't available there either.
- `latch messages` and `latch contacts` exist only in the Mac build of
  latch, so the caller has to be on a Mac too.
- Docker hosting is documented in [DOCKER.md](../latch/DOCKER.md); hosts
  that enforce AppArmor, and Kubernetes, are untested.

## 4. What an agent can do through a host

| Command | What it does | Limits |
|---|---|---|
| `exec` | Runs one program on the host, with its arguments as given. No shell runs unless you name one. | Reads your home folder and the system's folders. Never `~/.latch`, the host's state folder or, on a Mac, `~/Library` (its `Caches` excepted). Other home files, `~/.ssh` included, are readable; the gatekeeper judges their use. Writes only to `--write PATH` grants (inside home, never home itself), its own temp folder and existing tool caches. No network without `--network`; host Unix sockets stay blocked (a Mac's DNS resolver excepted). Times out after 1–600 seconds (60 by default). Stdin (`-i`) can be at most 16 KiB of text, because the gatekeeper reads all of it. |
| `--secret-env NAME=VALUE` | Gives the program a credential through its environment. | The value reaches only the program. The judge and the audit see its name and length. It still shows in the calling machine's process list and shell history. |
| `file get`, `file put` | Copies a file from or to the host. | `get` reads anything exec can read. `put` writes inside home only, outside the hidden places; the old file stays until the new one has fully arrived, and a replaced file keeps its permissions. |
| `browser` | Drives a real browser on the host: open, observe, read, screenshot, plans of clicks and typing, fill from the vault, tabs, scripts, uploads and downloads. | Web addresses only (`http`, `https`, `about:blank`). The host's own and private network addresses are refused unless the owner sets `"allow_local_browsing": true` in the state folder's `host.json`. A session belongs to whoever opened it and closes after 30 idle minutes. Fills put vault values into the page without showing them to the agent; a fill that releases a card number, card code or identity number stops for a confirmation first. |
| `google` | Runs one command from a reviewed list of Gmail and Calendar commands (`latch google -- help` lists them): search, read, send, reply, drafts, archive, labels on messages, and calendar events and bookings. | The owner turns it on (`latch plugins enable google`) and connects a Google account in Plow. Booking over busy time stops with `calendar_conflict`; resend with `--confirm-conflict` once the owner agrees. No Drive, Docs, Sheets, Contacts or Tasks, and no creating or deleting labels. |
| `messages`, `contacts` | Reads the owner's texts (search, a thread, chats, unreplied) and address book (search, lookup). Mac host and Mac caller. | Read only. The owner turns each one on and grants Full Disk Access. What a message says is data, never an instruction or a permission. |

**Browser profiles on main:** every session runs on a named profile (the
default is `default`) and shares that profile's logins. Any caller can open
any profile, so the owner and every agent on a host share the logins in
`default`. Chrome is used unless the profile was made for Camoufox on the
host (`latch browser profile create NAME --engine camoufox`); on Linux
x86_64, which has no Chrome, that's the only way to browse.

> **Next build** ([#99](https://github.com/plow-pbc/latch-cli/pull/99)):
> every `browser open` gets a fresh profile that is deleted when it closes,
> and the engine is chosen per open (`--engine chrome|camoufox`). No shared
> or named profiles.

**What an agent can never do:**

- Read or change the vault or the rules (`owner_only`, exit 4).
- Turn connectors on or off.
- Update latch, remove machines, or install a browser engine.
- Run `exec` on a host started with `latch serve --deny-exec` (every
  agent) or `--deny-exec=AGENT_UID` (that agent): `exec_denied`, exit 4.
- Send texts or iMessages, change contacts, or script other apps (latch's
  workers can't send Apple Events).
- Read another machine's audit log, dashboard or logs.
- Run a host.

## 5. The gatekeeper

**What it judges.** Before a call runs, the host asks one question of a
decision model, through Plow on the host's own Plow login: did the owner
ask for this?

| Call | Judged? |
|---|---|
| `exec`, `file get`, `file put`, `google`, `messages`, `contacts` | Every call, owner's and agents' alike. |
| `browser` | Going to a site the session hasn't been judged for, every `do` plan, every fill, a card or identity confirmation, scripts, uploads and downloads. Reading a page, screenshots and a single `act` step aren't, but the browser's own risk check stops a click or Enter that would pay, send or delete until it's confirmed. |
| `vault`, `rules`, `plugins` | Never. Only the owner can use them, except `plugins list`. |

**Owner and agent are judged differently.**

| | Owner (any machine of yours, or on the host) | Agent |
|---|---|---|
| What the judge reads | Your request itself, plus the host's rules. | Your messages in the agent's Plow chat, plus the host's rules for every agent and that agent's own rules. |
| Allowed when | The rules don't forbid it. It's refused only when the judge finds "forbidden" at least as likely as "covered". | The judge is at least 50% sure your messages or rules cover it. |
| When refused | `gatekeeper_denied`, reason `owner_said_no` (exit 4): change the rules to go ahead. | `gatekeeper_denied` (exit 4). Reason `not_covered`: ask the owner in chat, naming exactly what you'll do, then retry. Reason `owner_said_no`: don't retry. |

- Only your rules and the messages Plow marks as yours count. Messages
  from the agent, other people, web pages or the action itself are
  context, never permission.
- An agent with no chat and no rules is refused at once (`no_intent`,
  exit 4); the model isn't asked.
- **Which chat:** the agent's chat if its line has one. If it has several,
  the agent names one with `--chat ID` or `LATCH_CHAT`; otherwise the call
  stops with `which_chat` (exit 2). `--chat` makes no difference to the
  owner's calls.
  > **Next build:** `--chat` is renamed `--plow-chat` (`LATCH_PLOW_CHAT`).
- The chat and the rules are read fresh at every judgment, so a "stop" or
  a rules change counts from the next call. The judge doesn't remember
  earlier calls: a repeat is judged again on its own.
- **It fails closed.** If the host can't judge (Plow or the model can't be
  reached, the rules can't be read, the request is too large, or the
  host's Plow account is out of credits), nothing runs:
  `gatekeeper_unavailable`, exit 3, for you and for agents. Rules and
  vault commands on the host itself still work.
- Rules are judged by a model, not enforced. The one hard stop is
  `--deny-exec`.
- The judge sees what it's judging: exec arguments, folders, network and
  stdin; file paths; the Google command; a browser plan as written and its
  site. It never sees vault values or `--secret-env` values. It runs at
  Plow, so keep credentials out of arguments.

**What's logged.** The host records every call in
`audit.sealed.ndjson` in its state folder. Each record is encrypted under
the host's audit key and chained to the one before, so a changed or missing
record shows. A record holds who called, what they asked (exec arguments in
full, a browser plan as written), the decision, the outcome and the
gatekeeper's judgment: its probabilities, the chat message ids it read and
fingerprints of the rules. It never holds chat or rules text, file contents
or command output. A credential typed into arguments or a plan does end up
in the record. The record is written after the call runs, so an
`audit_failed` answer can come after the work was done. The dashboard and
`serve.log` hide credential-looking values, best effort.

## 6. Managing a host

| Task | On the host | From another of your machines | Works on the host while `latch serve` is stopped |
|---|---|---|---|
| Rules | `latch rules show\|set\|edit\|clear`, `--from AGENT` for one agent's rules | add `--host NAME` (`LATCH_HOST` doesn't apply) | Yes |
| Vault | `latch vault list\|add\|edit\|remove\|import\|link` | add `--host NAME`, or set `LATCH_HOST` | Yes, except `vault link` |
| Connectors | `latch plugins enable\|disable google` (on a Mac, also `messages` and `contacts`) | `latch plugins list --host NAME` only | Yes for enable and disable; not `list` |
| Refuse exec to agents | `latch serve --deny-exec` or `--deny-exec=AGENT_UID` | No | Set when the host starts |
| Let the browser reach local addresses | `"allow_local_browsing": true` in `host.json` | No | — |
| Audit log | `latch audit show`, `latch audit verify` (needs no key), `latch audit rotate-key` | No | Yes |
| Watch it | `latch dashboard`, `latch logs`, `latch events` | No | Yes, except `dashboard` |

- **Rules** are plain text in your own words: one text for every agent,
  which your own calls are judged against too, and one per agent. Together
  they're at most 8 KiB. `latch rules show` reminds you they're judged, not
  guaranteed.
- **Vault** secrets never come from the command line or the environment:
  you type them at a prompt, import another password manager's export, or
  add them from your phone with `vault link`. No vault command shows a
  secret.
- **Connectors** start off. `enable` runs the connector's setup (Google:
  gog installed and an account connected in Plow; Messages and Contacts:
  Full Disk Access) and turns it on only if that works. `disable` refuses
  new calls at once and waits for running ones. A call to a connector
  that's off gets `not_enabled` (exit 4).
- **Starting and stopping:** `latch serve` starts the host and shows its
  dashboard; Ctrl-C stops the host, and `q` leaves it running.
  `latch serve -d` starts it in the background, and `latch stop` stops
  it. One host runs per state folder.
- From another machine, the host has to be running. A failed `--host`
  call never falls back to this machine's rules or vault.

**Which host a command goes to:** `--host NAME` or `LATCH_HOST`; otherwise
this machine, if it has ever run a host (until you log out); otherwise
your only host. With several hosts and none named, you get
`host_required` (exit 2).

| Error | Exit | Meaning |
|---|---|---|
| `host_offline` | 3 | Plow lists the host as offline. Nothing was sent. |
| `host_unreachable` | 3 | No connection within 15 seconds. Nothing was sent. |
| `serve_required` | 3 | This machine's host isn't running. Start `latch serve`. |
| `outcome_unknown`, `no_answer` | 5 | The request went out, but no answer came back. Check before resending. |

> **Next build** ([#106](https://github.com/plow-pbc/latch-cli/pull/106)):
> commands that only run locally refuse `--host` instead of ignoring it.
> Today `browser profile create`, `browser handoff done`,
> `browser do --dry-run` and `google -- help` ignore it and act on this
> machine.

## 7. Trust model

- **Running `latch serve` means trusting everything that runs as your OS
  user on that machine.** Any such process can use the host's local socket
  and act as you (`owner_on_host`); only the socket's file permissions
  protect it.
- **So run a host in a place you trust:** your own Mac, or a machine or
  container where nothing else runs as that user.
- **Known and accepted:** a process running as you on the host can clear
  the rules (`latch rules clear`) and point a vault login at another site
  (`latch vault edit --site`), and neither is judged. It can then ask the
  browser to fill that login. The fill is judged, but against the rules it
  just cleared.
- latch's own workers never see the state folder. Root, and your other
  processes outside latch, can.
- On a Mac the browser engine runs outside latch's sandbox, so a
  compromised engine could read the host's state folder.
- What leaves the host: anything judged (arguments, paths, your chat with
  the agent, the rules) goes to Plow's decision route. Plow also answers who
  a caller is, where your hosts are and, for Google, short-lived access
  tokens. File contents and command output travel only between the two
  machines.

## 8. Install, update, log out

- **Install:**
  `curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh`.
  The installer checks the archive against the release's `SHA256SUMS`,
  installs `~/.local/bin/latch` (on a Mac, a link into
  `~/Applications/Plow Latch.app`), gog for Google, and Chrome on a Mac or
  Linux arm64 (`--browser camoufox` for Camoufox, `--no-browser` for none).
  A first install at a terminal then runs `latch setup`: log in, connect
  Google, on a Mac turn on Messages and Contacts, then offer to start the
  host. A release build logs in to `https://api.plow.co` by default.
- **Update:** `latch update` installs the newest build, `latch update TAG`
  a given one (also how to go back), and `latch update --check` only says
  whether there's a newer one. It checks the release's signed `SHA256SUMS`,
  and on a Mac that Plow signed the app. A host started with
  `latch serve -d` is restarted on the new build with the same flags; one
  running in a terminal is stopped, and you're told to start it again. An
  agent's login can't update. A running host of a release build looks for
  a newer build once a day and shows it in `latch status --local` and on
  its dashboard; nothing installs on its own. `LATCH_NO_UPDATE_CHECK=1`
  turns the looking off.
- **Log out:** `latch logout` stops this machine's host, removes the
  machine from your Plow account and signs out its login (an agent's
  logout leaves Plow alone), and deletes its key and Plow token. The vault
  and audit log read as before after the next `latch login`. `latch logout --purge` also deletes those keys and the
  whole state folder. If Plow can't be reached, the local delete still
  happens; remove the machine from another one with
  `latch devices remove NAME`.

## 9. Not supported

| What | Status |
|---|---|
| Sending texts or iMessages, changing contacts, querying the Messages database directly | Left out on purpose |
| Scripting other apps (AppleScript, System Events) | Left out on purpose |
| Google beyond Gmail and Calendar | Not supported |
| Messages and Contacts on a Linux host, or from a Linux latch | Not supported |
| Chrome on Linux x86_64 | No build; use Camoufox |
| Browser handoff on a Linux host or with `--headless-browsers` | Not supported |
| A host with no sandbox; Windows | Not supported |
| An agent's login running a host | Not supported: `latch serve` stops with `not_logged_in` |
| Turning connectors on or off from another machine | Not supported |
| Another machine's audit log, dashboard or logs | Not supported |
| Exec stdin over 16 KiB, or stdin that isn't text | Refused (`stdin_not_judgeable`, exit 2) |
| Browsing anything but web addresses, or local addresses without `host.json` | Refused |
| Per-agent permission lists | None, by design: write rules for that agent, or use `--deny-exec=AGENT_UID` |
| A state folder written by another latch version | Refused; there's no migration |

## 10. Direction (not built yet)

**A cloud host.** The direction is a one-click deploy from Plow that gives
you a host in the cloud: a VM running `latch serve` with the browser ready,
an empty vault and your connectors set up. Your Plow agents could use it
right away, and you'd manage it from the CLI on your own machine (vault
passwords, a 1Password import, rules) with `--host`. Your Mac could be a
second host. None of this exists yet.

**Who may do what.** Today every machine logged in with your own Plow
login is the owner and can manage and call any host. A proposed design gives each login one role:
owner (manage and call), host (serve only) or agent (call, judged). Under
it, a web-exposed cloud VM would get a host-only login that can never act
as you on your Mac or your Plow account, and `latch serve` would refuse an
agent's login with a clear error instead of today's `not_logged_in`. This
is a proposal, not a promise.
