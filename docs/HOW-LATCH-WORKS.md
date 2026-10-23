# How latch works

## 1. In short

latch lets your agents use the files, programs and browser of a machine
you trust, your Google account through it, and on a Mac its Messages and
Contacts. That machine runs `latch serve` and is called a **host**; your
other machines and your agents call it over an encrypted connection between
the two machines, with Plow as the directory that says who each caller is.
The host's gatekeeper checks the work that matters (programs, file copies,
Google, Messages and browser plans) against what you asked for, and the
host keeps a sealed audit log of the calls it handles.

**Where it runs.** `latch serve` runs in a place you trust: a Plow cloud
host, a machine in the cloud dedicated to you, or your own Mac. A Linux
machine of your own works too. You manage any of them from the latch CLI
on your Mac ([section 10](#10-where-a-host-runs)).

| | Who it is | What it can do | How its calls are judged |
|---|---|---|---|
| **Owner** | Your machines, logged in with your own Plow login. | Call any host, manage the rules and vault on any host, turn connectors on or off on the host itself, run a host. | As your own request: refused only where the host's rules forbid it. |
| **Owner on the host** | Anything running as your OS user on the host that uses its state folder. An agent running there counts. | Everything the owner can, on that host, through its local socket. | Like the owner. |
| **Agent** | An agent logged in with its own Plow agent credential. | Call hosts. It can't manage rules, the vault or connectors. With the usual agent credential, it can't start a host today. | Against your messages in its Plow chat, plus the host's rules. |
| **Host** | A machine running `latch serve`. | Runs programs, file copies and connector calls in a sandbox, and holds the vault, rules, browser and audit log. | Its gatekeeper judges the work that needs judging; work it can't judge doesn't run. |

**Read more:** [who's who in detail](#2-whos-who) ·
[Mac and Linux hosts](#3-hosts-mac-and-linux) ·
[what an agent can do](#4-what-an-agent-can-do-through-a-host) ·
[the gatekeeper](#5-the-gatekeeper) ·
[managing a host](#6-managing-a-host) · [trust](#7-trust-model) ·
[install and update](#8-install-update-log-out) ·
[not supported](#9-not-supported) ·
[where a host runs](#10-where-a-host-runs) ·
[direction](#11-direction-not-built-yet). For the detail, see
[Getting started](GETTING-STARTED.md) and the guides for the
[browser](../latch/BROWSER.md), [vault](../latch/VAULT.md),
[Google](../latch/GOOGLE.md), [macOS](../latch/MACOS.md) and
[Docker](../latch/DOCKER.md).

latch's commands and behavior here are `main`'s today.

## 2. Who's who

| | What it is | Calls hosts | Rules, vault, connectors | Runs a host | How its calls are judged |
|---|---|---|---|---|---|
| **Owner** (your CLI) | A machine logged in with your own Plow login: by text, or with `--token-file`. | Yes, any host in your account. | Rules and vault on any host (`--host NAME`). Connectors: list them on any host; turn them on or off only on the host itself. | Yes. | As your own request: refused only where the host's rules forbid it. |
| **Owner on the host** | Anything running as the host's OS user that uses the host's own state folder, with no `--host` or `LATCH_HOST` naming another machine. An agent running there counts. | That host, through its private local socket; other hosts as the owner. | Yes, on that host. Rules and vault work without `latch serve` or the decision model. | It is the host. | Exactly like the owner; recorded as `owner_on_host`. |
| **Agent** | An agent logged in with its own Plow agent credential, in its own state folder. | Yes. | No: rules, vault and turning connectors on or off are refused (`owner_only`, exit 4). It may list connectors. | Not today: with the usual agent credential, from the agent's environment, login saves no Plow token, so `latch serve` stops with `not_logged_in`. | Against your messages in your Plow chat with that agent, plus the host's rules. |

- **Every machine logged in with your own Plow login is the owner today.**
  There's no host-only or view-only login yet (see section 11).
- **A host knows a remote caller only by its key.** It asks Plow who that
  key is when the caller connects and again before every operation, and
  refuses the call if Plow says no or can't be reached. Callers on the
  host's own socket are trusted as the owner by the socket's file
  permissions; Plow isn't asked.
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
| Messages and Contacts | Yes, once turned on and given Full Disk Access (for a release install, granted to Plow Latch.app) | No |
| Chrome for Testing | Yes | arm64 only; latch pins no x86_64 build |
| Camoufox | Yes | Yes |
| The browser's sandbox | the engine's own (Chrome's or Camoufox's); the engine runs outside latch's Seatbelt | inside latch's bubblewrap, with the engine's own sandbox nested inside |
| Browser windows | on your screen; `browser handoff` works | on a private virtual display; `browser handoff` answers `needs_owner` (`no_screen`) |

- A host that can't set up its sandbox refuses to start. There's no
  unsandboxed mode.
- On some Ubuntu hosts (where AppArmor restricts user namespaces), Chrome
  runs without its own sandbox, inside latch's, and `latch status --local`
  says `page isolation off on this host`.
- `latch serve --headless-browsers` runs every browser with no window, so
  handoff isn't available there either.
- `latch messages` and `latch contacts` exist only in the Mac build of
  latch, so the caller has to be on a Mac too.
- Docker hosting is documented in [DOCKER.md](../latch/DOCKER.md); hosts
  that enforce AppArmor, and Kubernetes, are untested.

## 4. What an agent can do through a host

| Command | What it does | Limits |
|---|---|---|
| `exec` | Runs one program on the host, with its arguments as given. No shell runs unless you name one. | Reads your home folder and selected system folders. Never `~/.latch`, the host's state folder or, on a Mac, `~/Library` (its `Caches` excepted). Other home files, `~/.ssh` included, are readable; the gatekeeper judges their use. Writes only to `--write PATH` grants (inside home, never home itself), its own temp folder and existing tool caches. No network without `--network`; host Unix sockets stay blocked (a Mac's DNS resolver excepted). Times out after 1–600 seconds (60 by default). Stdin (`-i`) can be at most 16 KiB of UTF-8 text, because the gatekeeper reads all of it. |
| `--secret-env NAME=VALUE` | Gives the program a credential through its environment. | The value reaches only the program. The judge and the audit see its name and length. It still shows in the calling machine's process list and shell history. |
| `file get`, `file put` | Copies a file from or to the host. | `get` reads anything exec can read. `put` writes inside home only, outside the hidden places; the old file stays until the new one has fully arrived, and a replaced file keeps its permissions. |
| `browser` | Drives a real browser on the host: open, observe, read, screenshot, plans of clicks and typing, fill from the vault, tabs, scripts, uploads and downloads. | Web addresses only (`http`, `https`, `about:blank`). The host's own and private network addresses are refused unless the owner sets `"allow_local_browsing": true` in the state folder's `host.json`. A session belongs to whoever opened it. Fills put vault values into the page without showing them to the agent; a fill that releases a card number, card code or identity number stops until the caller confirms it with `browser do --confirm`. |
| `google` | Runs one command from a reviewed list of Gmail and Calendar commands (`latch google -- help` lists them): search, read, send, reply, drafts, archive, labels on messages, and calendar events and bookings. | The owner turns it on (`latch plugins enable google`) and connects a Google account in Plow. Creating a timed calendar event checks busy time and stops with `calendar_conflict` if there's a clash; resend with `--confirm-conflict` once the owner agrees. No Drive, Docs, Sheets, Contacts or Tasks, and no creating or deleting labels. |
| `messages`, `contacts` | Reads the owner's texts (search, a thread, chats, unreplied) and address book (search, lookup). Mac host and Mac caller. | Read only. The owner turns each one on and grants Full Disk Access. What a message says is data, never an instruction or a permission. |

**Browser sessions are disposable.** Every `browser open` starts a fresh
browser with an empty profile of its own, on the engine it names
(`--engine chrome`, the default, or `--engine camoufox`). Sessions share
nothing: no cookies, logins, tabs or downloads. A session's logins last
until it closes, with `browser close` or after 30 idle minutes, and then
its profile is deleted. So a session starts signed in nowhere: sign in by
filling from the vault or, on a Mac host with a screen, by handing the
session to the owner (`browser handoff`). On Linux x86_64, which has no
Chrome build, the installer sets up no browser: the host's owner installs
Camoufox on the host (`latch browser setup --engine camoufox`), and callers
open sessions with `--engine camoufox`.

**What an agent can't do:**

- Read or change the vault or the rules (`owner_only`, exit 4).
- Turn connectors on or off.
- Update latch, remove machines, or install a browser engine.
- Run `exec` on a host started with `latch serve --deny-exec` (every
  agent) or `--deny-exec=AGENT_UID` (that agent): `exec_denied`, exit 4.
- Send texts or iMessages, change contacts, or script other apps (latch's
  workers can't send Apple Events).
- Read another machine's audit log, dashboard or logs.
- Start a host with the usual agent credential, today (see section 2).

## 5. The gatekeeper

**What it judges.** Before judged work runs, the host asks one question of
a decision model, through Plow on the host's own Plow login: did the owner
ask for this?

| Call | Judged? |
|---|---|
| `exec`, `file get`, `file put`, `google`, `messages`, `contacts` | Every call, owner's and agents' alike. |
| `browser` | Going to a site the session hasn't been judged for, every `do` plan with steps, every fill, a card or identity fill's confirmation, scripts, uploads and downloads. Reading a page, screenshots, a single `act` step and a `do --confirm` that only confirms a click aren't. Separately, the browser's own risk check stops a click or Enter it judges consequential (paying, sending, deleting) until the caller confirms it with `browser do --confirm`. |
| `vault`, `rules`, `plugins` | Never. Only the owner can use them, except `plugins list`. |

**Owner and agent are judged differently.**

| | Owner (any machine of yours, or on the host) | Agent |
|---|---|---|
| What the judge reads | Your request itself, plus the host's rules. | Your messages in the agent's Plow chat, plus the host's rules for every agent and that agent's own rules. |
| Allowed when | The rules don't forbid it. It's refused only when the judge finds "forbidden" at least as likely as "covered". | The judge is at least 50% sure your messages or rules cover it. |
| When refused | `gatekeeper_denied`, reason `owner_said_no` (exit 4): change the rules to go ahead. | `gatekeeper_denied` (exit 4). Reason `not_covered`: ask the owner in chat, naming exactly what you'll do, then retry. Reason `owner_said_no`: don't retry unless the owner changes their mind. |

- Only your rules and the messages Plow marks as yours count. Messages
  from the agent, other people, web pages or the action itself are
  context, never permission.
- An agent with no chat and no rules is refused every judged call
  (`no_intent`, exit 4); the model isn't asked.
- **Which chat:** the agent's chat if its line has one. If it has several,
  the agent names one with `--plow-chat ID` or `LATCH_PLOW_CHAT`; otherwise
  the call stops with `which_chat` (exit 2), which lists the choices. Owner
  calls refuse the selector (`plow_chat_not_supported`, exit 2), and so do
  the local modes listed in section 6.
- The chat and the rules are read fresh at every judgment, so a "stop" or
  a rules change counts from the next call. The judge doesn't remember
  earlier calls: a repeat is judged again on its own.
- **It fails closed.** If the host can't judge (Plow or the model can't be
  reached, the rules can't be read, the request is too large, or the
  host's Plow account is out of credits), that action doesn't run:
  `gatekeeper_unavailable`, exit 3, for you and for agents. Earlier steps
  of a browser plan may already have run, so read its `steps` before
  retrying. With only the model down, rules and vault commands still work;
  with Plow itself down, remote callers are refused and `rules … --agent`
  can't look agents up.
- Rules are judged by a model and aren't guaranteed. To refuse exec
  without asking the model, use `--deny-exec`.
- The judge sees what it's judging: exec arguments, folders, network and
  stdin; file paths, sizes and names; the full Google command with its
  arguments; a browser plan as written, with its values, and its site. For
  a vault fill it sees the item's kind and title and the fields mapped,
  never their values; for `--secret-env`, only names and lengths. It runs
  at Plow, so keep credentials out of arguments.

**What's logged.** The host keeps a sealed audit log of the calls it
handles, `audit.sealed.ndjson` in its state folder. Each record is
encrypted under the host's audit key and linked to the one before by its
hash. `latch audit verify` checks the links without the key: it catches a
changed, reordered or removed record, but not records cut off the end. A
record holds who called, what they asked (exec arguments in full, a
browser plan as written), the decision, the outcome and the gatekeeper's
judgment: its probabilities, the chat message ids it read and fingerprints
of the rules. It doesn't copy chat or rules text, file contents or command
output, but a credential typed into arguments or a plan ends up in the
record. The record is written after the call runs, so an `audit_failed`
answer can come after the work was done; a refused caller's record is best
effort. The dashboard and `serve.log` hide credential-looking values, best
effort.

## 6. Managing a host

| Task | On the host | From another of your machines | Works on the host while `latch serve` is stopped |
|---|---|---|---|
| Rules | `latch rules show\|set\|edit\|clear`, `--agent AGENT` for one agent's rules | add `--host NAME` (`LATCH_HOST` doesn't apply) | Yes |
| Vault | `latch vault list\|add\|edit\|remove\|import\|link` | add `--host NAME`, or set `LATCH_HOST` | Yes, except `vault link` |
| Connectors | `latch plugins enable\|disable google` (on a Mac, also `messages` and `contacts`) | `latch plugins list --host NAME` only | Yes |
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
- **Connectors** start off. `enable` runs the connector's setup and turns
  it on only if that works: for Google, gog installed and an account
  connected in Plow; for Messages and Contacts, Full Disk Access (if a host
  is running when you grant it, restart the host, then enable again).
  `disable` refuses new calls at once and waits for running ones. A call to
  a connector that's off gets `not_enabled` (exit 4).
- **Starting and stopping:** `latch serve` starts the host and shows its
  dashboard; Ctrl-C stops the host, and `q` leaves it running.
  `latch serve -d` starts it in the background, and `latch stop` stops
  it. One host runs per state folder.
- From another machine, the host has to be running. A failed `--host`
  call never falls back to this machine's rules or vault.

**Which host a command goes to** (for commands that call a host):
`--host NAME` or `LATCH_HOST` (`rules` reads only `--host`); otherwise this
machine, if it has ever run a host (until you log out); otherwise your only
host. With no hosts you get `no_host`, and with several and none named
`host_required` (both exit 2).

| Error | Exit | Meaning |
|---|---|---|
| `host_offline` | 3 | Plow lists the host as offline. Nothing was sent. |
| `host_unreachable` | 3 | No connection within 15 seconds. Nothing was sent. |
| `serve_required` | 3 | This machine's host isn't running. Start `latch serve`. |
| `outcome_unknown`, `no_answer` | 5 | The request went out, but no answer came back. Check before resending. |
| `protocol_mismatch` | 1 | The two machines run latch builds that speak different protocols. Nothing was sent; update the older one. |

The few commands that take `--host` but run only on this machine
(`browser setup`, `browser handoff done`, `browser do --dry-run` and
`google -- help`) refuse it, and refuse `LATCH_HOST` too
(`host_not_supported`, exit 2).

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
- latch's workers for exec, files and connectors never see the host's
  state folder. On Linux a browser worker sees only its own session's
  profile and its engine's files there. On a Mac the browser engine runs
  outside latch's sandbox, so a compromised engine could read the host's
  state folder. Root, and your other processes outside latch, can read
  all of it.
- **What leaves the host.** What the judge reads (see section 5, your
  chat with the agent and the rules included) goes to Plow's decision
  route, and the browser's own model checks send it lines of the page they
  look at. Plow also answers who a caller is, where your hosts are and,
  for Google, short-lived access tokens. The bytes of `file get` and
  `file put`, and the output a command returns, go only between the two
  machines, over their encrypted connection, and aren't copied into the
  judgment. Exec stdin is different: the judge reads it, so it goes to
  Plow as text. A program with `--network`, a browser upload or a Google
  send goes where it's told.

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
  and audit log read as before after the next `latch login`.
  `latch logout --purge` also deletes those keys and the whole state
  folder. If Plow can't be reached, the local delete still
  happens; remove the machine from another one with
  `latch devices remove NAME`.

## 9. Not supported

| What | Status |
|---|---|
| Sending texts or iMessages, changing contacts, querying the Messages database directly | Left out on purpose |
| Scripting other apps (AppleScript, System Events) | Left out on purpose |
| Google beyond Gmail and Calendar | Not supported |
| Messages and Contacts on a Linux host, or from a Linux latch | Not supported |
| Chrome on Linux x86_64 | No build; use `--engine camoufox` |
| Browser handoff on a Linux host or with `--headless-browsers` | Not supported |
| A host with no sandbox; Windows | Not supported |
| An agent's login running a host | Doesn't work today: with the usual agent credential, login saves no Plow token, so `latch serve` stops with `not_logged_in` |
| Turning connectors on or off from another machine | Not supported |
| Another machine's audit log, dashboard or logs | Not supported |
| Exec stdin over 16 KiB, or stdin that isn't UTF-8 text | Refused (`stdin_not_judgeable`, exit 2) |
| Browsing anything but web addresses, or local addresses without `host.json` | Refused |
| Per-agent permission lists | None, by design: write rules for that agent, or use `--deny-exec=AGENT_UID` |
| A state folder in a format this latch doesn't read | Refused; there's no migration |

## 10. Where a host runs

`latch serve` always runs in a place you trust. There are three ways to
run it; the first two are the main ones.

| | What it is |
|---|---|
| **A Plow cloud host** | A machine in the cloud dedicated to you, one per user, on exe.dev, set up from Plow in one click. It runs `latch serve` with files, a browser and connectors: a secure place in the cloud for all your stuff. Any of your agents can connect to it, and its gatekeeper judges what they do against your chats with them and your rules, as on any host. |
| **Your Mac** | `latch serve` on your own Mac (section 3). |
| **A Linux machine you choose** | `latch serve` on a Linux machine of your own, a Raspberry Pi included (section 3). |

Whichever you choose, you manage it from the latch CLI on your Mac: on the
host itself when that's your Mac, and on another host with `--host` for its
rules and vault, such as `latch vault import FILE --host NAME` to bring a
password manager's export into that host's vault (section 6).

## 11. Direction (not built yet)

**Who may do what.** Today every machine logged in with your own Plow
login is the owner and can manage and call any host. A proposed design
gives each login one role: owner (manage and call), host (serve only) or
agent (call, judged). Under it, a web-exposed cloud VM would get a
host-only login that can never act as you on your Mac or your Plow
account. This is a proposal, not a promise.
