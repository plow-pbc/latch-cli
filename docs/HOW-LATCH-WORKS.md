# How Latch works

## 1. In short

Latch lets your agents use a machine you trust: its files, programs and
browser, your Google account through it, and on a Mac its Messages and
Contacts. That machine runs `latch serve` and is called a **host**.

Your other machines and your agents call the host over an encrypted
connection between the two machines. Plow is the directory: it says who
each caller is and where your hosts are. The bytes of a call never pass
through Plow.

The host's **gatekeeper** judges the work that matters (programs, file
copies, Google, Messages and browser plans) against your rules and what
you told the agent. The host keeps a sealed audit log of the calls it
handles.

**Contents:** [who's who](#2-whos-who) ·
[Mac and Linux hosts](#3-hosts-mac-and-linux) ·
[what an agent can do](#4-what-an-agent-can-do-through-a-host) ·
[the gatekeeper](#5-the-gatekeeper) ·
[managing a host](#6-managing-a-host) · [trust](#7-trust-model) ·
[install, update, log out](#8-install-update-log-out) ·
[not supported](#9-not-supported) ·
[where a host runs](#10-where-a-host-runs).

For step-by-step setup, see [Getting started](GETTING-STARTED.md). The
guides cover the [browser](../latch/BROWSER.md), [vault](../latch/VAULT.md),
[Google](../latch/GOOGLE.md), [macOS](../latch/MACOS.md) and
[Docker](../latch/DOCKER.md).

## 2. Who's who

| | What it is | Calls hosts | Rules, vault, connectors | Runs a host | How its calls are judged |
|---|---|---|---|---|---|
| **Owner** | One of your machines, logged in with your own Plow login: by text, or with `--token-file`. | Yes, any host in your account. | Rules and vault on any host (`--host NAME`). Connectors: listed on any host, turned on or off only on the host itself. | Yes. | As your own request: refused only where the host's rules forbid it. |
| **Owner on the host** | Anything running as the host's OS user that uses the host's own state folder, with no `--host` naming another machine. An agent running there counts. | That host, through its private local socket. Other hosts, as the owner. | Yes, on that host. Rules and vault work even while the host is stopped. | It is the host. | Exactly like the owner. The audit records it as `owner_on_host`. |
| **Agent** | An agent logged in with its own Plow agent credential, in its own state folder. | Yes. | No: `owner_only` (exit 4). It may run `vault list`, which shows each item's id, title and saved sites, and `vault link --kind login`, which asks you for a login on your phone (it sees only whether it was saved, and the new item's id), and it sees which connectors each host has on. On its own computer, where latch is installed alone, it runs `latch update`. | No: its login saves no Plow token, so `latch serve` stops with `not_logged_in`. | Against your rules and your messages in your Plow chat with that agent. |

- **A host knows a remote caller only by its key.** It asks Plow who that
  key is when the caller connects, and again before every operation. If
  Plow says no or can't be reached, the call is refused. Callers on the
  host's own local socket are trusted as the owner: the socket's file
  permissions protect it, and Plow isn't asked.
- **An agent can't read a host's status or audit.** A host refuses an
  agent's `status --host`, `audit --host`, `audit judge --host` and
  `audit shot --host` with `owner_only` (exit 4).
- **While a machine's host runs, its calls to other hosts go through that
  host.** Both use the state folder's one key, so a command dialing on its
  own would cut the host off. A host that is starting or stopping takes no
  calls: they fail with `host_unreachable`. If another copy of the key
  connects (another machine with a copy of the folder, say), the host's log
  says so.
- **Never copy a machine's state folder or Plow token to where an agent
  runs.** The host would take the agent for you, and judge its calls only
  against your rules.

## 3. Hosts: Mac and Linux

| | Mac | Linux, including a Raspberry Pi |
|---|---|---|
| Supported | macOS 13 or later, Apple silicon and Intel | x86_64 and arm64 |
| Sandbox for exec, files, Google and Messages | Seatbelt | bubblewrap and seccomp; needs user namespaces |
| `latch serve` runs as | a launchd job, using the `latch` inside `Plow Latch.app` | a systemd user service |
| Key, Plow login, vault and audit keys | the Keychain, in the signed release app | files only you can read (mode 600) |
| Messages and Contacts | Yes, once turned on and given Full Disk Access (granted to Plow Latch.app) | No (`unsupported_platform`) |
| Google | macOS 15 or later | Yes |
| Chrome for Testing, Camoufox | Yes | Yes |
| The browser's sandbox | the engine's own; the engine runs outside latch's Seatbelt | latch's bubblewrap, with the engine's own sandbox inside it |
| Browser windows | on your screen; `browser handoff` works | a private virtual display, or none with `--headless-browsers`; no `browser handoff` (`needs_owner`, `no_screen`) |

- A host that can't set up its sandbox refuses to start. There is no
  unsandboxed mode.
- On some Ubuntu hosts, AppArmor restricts user namespaces. There Chrome
  runs without its own sandbox, inside latch's, and `latch status --local`
  says `page isolation off on this host`.
- `latch serve --headless-browsers` runs every browser with no window, so
  handoff isn't available there either.
- Docker hosting is described in [DOCKER.md](../latch/DOCKER.md). Hosts
  that enforce AppArmor, and Kubernetes, are untested.

## 4. What an agent can do through a host

| Command | What it does | Limits |
|---|---|---|
| `exec` | Runs one program on the host, with its arguments as given. No shell runs unless you name one. | Reads your home folder and the system's own files. Never reads `~/.latch`, the host's state folder or, on a Mac, `~/Library` (its `Caches` excepted). Other home files, `~/.ssh` included, are readable, and the gatekeeper judges their use. Writes only to `--write PATH` folders (inside home, never home itself), its own temp folder and existing tool caches. No network without `--network`; host Unix sockets stay blocked, except a Mac's DNS resolver. Times out after 1 to 600 seconds (60 by default). Stdin (`-i`) is at most 16 KiB of UTF-8 text, because the gatekeeper reads all of it. |
| `--secret-env NAME=VALUE` | Gives the program a secret through its environment. | Only the program sees the value. The gatekeeper and the audit see its name and length. It still shows in the calling machine's process list and shell history. |
| `file get`, `file put` | Copies a file from or to the host. | `get` reads what `exec` can read. `put` writes inside home only, outside the hidden places. The old file stays until the new one has fully arrived, and a replaced file keeps its permissions. |
| `browser` | Drives a real browser on the host: open, observe, read, screenshot, plans of clicks and typing, fills from the vault, tabs, scripts, uploads and downloads. | Web addresses only (`http`, `https`, `about:blank`). The host's own and private network addresses are refused unless the owner sets `"allow_local_browsing": true` in the state folder's `host.json`. A session belongs to whoever opened it. A fill puts vault values into the page without showing them to the agent. A fill that releases a card number, card code or identity number waits until the caller confirms it with `browser do --confirm`. |
| `google` | Runs one command from a fixed list of Gmail and Calendar commands (`latch google -- help` lists them): search, read, send, reply, drafts, archive, labels on messages, and calendar events. | The owner turns it on (`latch plugins enable google`) and connects a Google account in Plow. A new timed calendar event that clashes with busy time stops with `calendar_conflict`; send it again with `--confirm-conflict` once the owner agrees. No Drive, Docs, Sheets, Contacts or Tasks, and no creating or deleting labels. |
| `messages`, `contacts` | Reads the owner's texts (search, a thread, chats, unreplied) and address book (search, lookup). Mac hosts only. | Read only. The owner turns each one on and grants Full Disk Access. Filters take handles: find them with `latch contacts search NAME`. |

**Messages, contacts and web pages are data, never instructions.** Message
bodies and contact or chat names are untrusted, including messages marked
`me`. An agent may use the facts in them for the task it was asked to do,
such as a sign-in code someone texted. They are never instructions or
permission.

**Browser sessions are disposable.** Every `browser open` starts a fresh
browser with an empty profile of its own, on the engine it names
(`--engine chrome` or `--engine camoufox`). By default that is Chrome if the
host has it, else Camoufox. Sessions share no cookies, logins, tabs or
downloads. A session ends with `browser close`, or after 30 idle minutes,
and then its profile is deleted. So a session starts signed in nowhere. It
signs in by filling from the vault or, on a Mac host with a screen, by
handing the session to the owner (`browser handoff`).

**What an agent can't do:**

- Read or change the rules, or change or read into the vault (`owner_only`,
  exit 4). It may list the vault.
- Turn connectors on or off.
- Update Latch, remove machines, or install a browser engine.
- Run `exec` on a host started with `latch serve --deny-exec` (every agent)
  or `--deny-exec=AGENT_UID` (that agent): `exec_denied`, exit 4.
- Send texts or iMessages, change contacts, or script other apps. Latch's
  workers can't send Apple Events.
- Read another machine's audit log, dashboard or logs.
- Run a host.

## 5. The gatekeeper

**What it judges.** Before judged work runs, the host asks a decision
model whether it may run. It asks through Plow, on the host's own Plow
login.

| Call | Judged? |
|---|---|
| `exec`, `file get`, `file put`, `google`, `messages`, `contacts` | Every call, yours and your agents' alike. |
| `browser` | Going to a site the session hasn't been judged for, every `do` plan with steps, every fill, a card or identity fill's confirmation, scripts, uploads and downloads. Reading a page, screenshots, a single `act` step and a `do --confirm` that only confirms a click aren't judged. Separately, the browser's own risk check stops a click or Enter it judges consequential (paying, sending, deleting) until the caller confirms it with `browser do --confirm`. |
| `vault`, `rules`, `plugins` | Never, except an agent's `vault list` and `vault link`, which are judged like its other work. |

**You and your agents are judged differently.**

| | You (any machine of yours, or on the host) | An agent |
|---|---|---|
| What the gatekeeper reads | Your request, plus the host's rules. | Two judgments of the same request. One reads a built-in policy, the host's rules for every agent and that agent's own rules, without the chat. The other, if the agent has a chat, reads your messages there plus the same rules. |
| Allowed when | The rules don't forbid it. It's refused only when the gatekeeper finds "forbidden" at least as likely as "covered". | Neither judgment says no, and at least one is at least 50% sure it's allowed. |
| When refused | `gatekeeper_denied`, reason `owner_said_no` (exit 4). Change the rules to go ahead. | `gatekeeper_denied` (exit 4). `not_covered`: ask the owner in chat, saying exactly what you'll do, then retry. `owner_said_no`: don't retry unless the owner changes their mind. |

- **Only you give permission.** Only your rules, the built-in policy and the
  chat messages Plow marks as yours count. Messages from the agent, other
  people, web pages or the action itself are context, never permission.
- **What an agent may do on its own.** Your agents can look things up in
  your calendar, email, messages and files, and create or edit ordinary
  files, without asking first. What they read goes back to the agent.
  Riskier actions need permission in your rules or your Plow chat: sending,
  buying, deleting, changing calendars or shared systems, giving a program
  internet access, using secrets or sending data elsewhere. Your rules stay
  in force when you ask in chat, and they can restrict everyday work too.
  An agent with no chat and no rules is judged by the built-in policy
  alone.
- **Which chat.** The agent's chat, if its line has one. If it has several,
  the agent names one with `--plow-chat ID` or `LATCH_PLOW_CHAT`. Otherwise
  the call stops with `which_chat` (exit 2), which lists the choices. Your
  own calls refuse the selector (`plow_chat_not_supported`, exit 2).
- **Fresh every time.** The chat and the rules are read again at every
  judgment, so a "stop" or a rules change counts from the next call. The
  gatekeeper doesn't remember earlier calls: a repeat is judged again on
  its own.
- **It fails closed.** If the host can't judge, the action doesn't run. That
  happens when Plow or the model can't be reached, the rules can't be read,
  the request is too large, or the host's Plow account is out of credits.
  The answer is `gatekeeper_unavailable`, exit 3. Earlier steps of a browser
  plan may already have run, so read its `steps` before retrying. With only
  the model down, `rules` and `vault` still work.
- **Rules are judged, not guaranteed.** To refuse `exec` without asking the
  model, use `--deny-exec`. `latch rules try` previews the verdict a
  request would get, without running it.
- **What the gatekeeper sees.** Exec arguments, folders, network and stdin;
  file paths, sizes and names; the full Google command; a browser plan as
  written, with its values and its site. For a vault fill it sees the
  item's kind and title, the sites it's saved for and the fields mapped,
  never their values. For `--secret-env` it sees only names and lengths.
  The judgment runs at Plow, so keep credentials out of arguments.
- **Signing in with the vault.** A saved login fills only on its own sites,
  and the agent never receives its values. Its site says where a login may
  go, not that it may be used: signing in still needs your OK, in a rule or
  the chat. Asking for something to be done on a site covers signing in
  there.

**The audit log.** The host keeps a sealed log of the calls it handles,
`audit.sealed.ndjson` in its state folder.

- Each record is encrypted under the host's audit key and linked to the one
  before by its hash. `latch audit verify` checks the links without the
  key. It catches a changed, reordered or removed record, but not records
  cut off the end.
- A record holds who called, what they asked (exec arguments in full, a
  browser plan as written), the decision, the outcome and the gatekeeper's
  judgment. It doesn't copy chat or rules text, file contents or command
  output. A credential typed into arguments or a plan does end up in it.
- The record is written after the call runs, so an `audit_failed` answer
  can come after the work was done.
- `latch audit`, the dashboard and the host's log hide credential-looking
  values in commands, best effort: some can slip through.
- A browser plan's record names the screenshots the host kept, one after
  each action, for 7 days. `latch audit shot` shows them. Latch takes none
  while a vault value is on the page, but typed text and the rest of the
  page can show.
- The host also keeps each gatekeeper request and Plow's answer for 7 days,
  sealed the same way. `latch audit judge ID` shows one, ID being the
  record's `kept_request`. They never hold a vault value, and agents can't
  read them.

## 6. Managing a host

| Task | On the host | From another of your machines | Works while the host is stopped |
|---|---|---|---|
| Rules | `latch rules show\|set\|edit\|clear\|try`, `--agent AGENT` for one agent's rules | add `--host NAME` | Yes |
| Vault | `latch vault list\|show\|add\|edit\|remove\|import\|link\|link-state` | add `--host NAME` | Yes, except `vault link` and `link-state` |
| Connectors | `latch plugins enable\|disable google` (on a Mac, also `messages` and `contacts`) | `latch status` shows the ones each host has on; `latch status --host NAME` lists them all | Yes |
| Refuse exec to agents | `latch serve --deny-exec` or `--deny-exec=AGENT_UID` | No | Set when the host starts |
| Keep a Mac awake | `latch settings set --keep-awake on`: no idle sleep while the host serves on AC power. The display still sleeps and locks, and closing the lid still sleeps the Mac. | No | Yes |
| Audit log | `latch audit` (`--before ID`, `--all`, `--follow`), `latch audit judge ID`, `latch audit shot ID N --output FILE`, `latch audit verify`, `latch audit rotate-key` | `audit`, `audit judge` and `audit shot` with `--host NAME`; `verify` and `rotate-key` only on the host | Yes |
| Watch it | `latch dashboard`, `latch logs [-f]`, `latch audit --follow`, `latch status --local --follow` | `latch audit --follow --host NAME`, `latch status --host NAME --follow` | Yes, except `dashboard` |

- **Rules** are plain text in your own words. There is one text for every
  agent, which your own calls are judged against too, and one per agent.
  Together they are at most 8 KiB.
- **Vault** secrets never come from the command line or the environment.
  You type them at a prompt, import another password manager's export, or
  add them from your phone with `vault link`. See [VAULT.md](../latch/VAULT.md).
- **Connectors** start off. `enable` runs the connector's setup and turns it
  on only if that works. For Google, that means an account connected in
  Plow. For Messages and Contacts, it means Full Disk Access: if the host is
  running when you grant it, restart the host, then enable again. `disable`
  refuses new calls at once and waits for running ones. A call to a
  connector that's off gets `not_enabled` (exit 4).
- **Starting and stopping.** `latch serve` starts the host and shows its
  dashboard. Ctrl-C stops the host, and `q` closes only the dashboard.
  `latch serve -d` starts it in the background, and `latch stop` stops it.
  `latch serve --foreground` runs it in the terminal's own process, logging
  to stderr, for CI, tests and containers. One host runs per state folder:
  `--state-dir DIR`, else `$LATCH_STATE_DIR`, else `~/.latch`.
- **The dashboard** lists each call and how it went: Done, Running, Refused
  (by the gatekeeper or your settings), Stopped (by a browser safety check)
  or Failed. Enter opens a call, and Tab switches between its steps, the
  gatekeeper's decision and the request. `/` filters, `r` shows only
  problems, and Space pauses.
- **The host's log** is `serve.log`, one JSON line per record. Past 10 MB,
  the next start keeps it as `serve.log.1` and begins a new one.
- **The Mac app.** `latch app` opens Plow Latch.app, the app this `latch`
  is inside. `latch setup --status` lists setup's steps as JSON, for an
  app. [APP.md](../latch/APP.md) describes the JSON an app reads.
- From another machine, the host has to be running. A failed `--host` call
  never falls back to this machine's rules or vault.

**Which host a command goes to.** `--host NAME`, or `LATCH_HOST`. (`rules`,
`audit` and `status` read only `--host`.) Otherwise this machine, if it has
ever run a host. Otherwise your only host. With no hosts you get `no_host`,
and with several and none named `host_required` (both exit 2).

| Error | Exit | Meaning |
|---|---|---|
| `host_offline` | 3 | Plow doesn't list the host as online. Nothing was sent. |
| `host_unreachable` | 3 | No connection within 15 seconds, or it failed. Nothing was sent. |
| `serve_required` | 3 | This machine's host isn't running. Start `latch serve`. |
| `outcome_unknown`, `no_answer` | 5 | The request went out, but no answer came back. Check before you send it again. |
| `protocol_mismatch` | 1 | The two machines run versions of Latch that speak different protocols. Nothing was sent. Update the older one. |

A few commands run only on this machine: `browser setup`,
`browser handoff done`, `browser do --dry-run` and `google -- help`. They
refuse `--host` and `LATCH_HOST` (`host_not_supported`, exit 2).

## 7. Trust model

- **Running a host means trusting everything that runs as your OS user on
  that machine.** Any such process can use the host's local socket and act
  as you (`owner_on_host`). Only the socket's file permissions protect it.
  That includes changing the rules and the vault, which are never judged.
- **So run a host in a place you trust:** your own Mac, or a machine or
  container where nothing else runs as that user.
- Latch's workers for exec, files and connectors never see the host's state
  folder. On Linux a browser worker sees only its own session's profile and
  its engine's files. On a Mac the browser engine runs outside latch's
  sandbox, so a compromised engine could read the host's state folder.
  Root, and your other processes outside Latch, can read all of it.
- **What leaves the host.** What the gatekeeper reads goes to Plow's
  decision route: the request, your rules, and your chat with the agent
  (see section 5). The browser's own risk checks send it lines of the page
  they look at. Plow also answers who a caller is, where your hosts are
  and, for Google, short-lived access tokens.
- **What stays between the two machines.** The bytes of `file get` and
  `file put`, and a command's output, go only between the two machines,
  over their encrypted connection. Exec stdin is different: the gatekeeper
  reads it, so it goes to Plow as text. A program with `--network`, a
  browser upload or a Google send goes where it's told.

For how to report a vulnerability, see [SECURITY.md](../SECURITY.md).

## 8. Install, update, log out

- **Install:**
  `curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh`.
  See [Getting started](GETTING-STARTED.md#1-install) for what it puts
  where and its options. A release logs in to `https://api.plow.co`. The
  macOS app is signed with Plow's Developer ID and notarized by Apple.
  - It checks the release's `SHA256SUMS` against Latch's release signing
    key, which it carries, and on a Mac that Plow signed the app, before it
    runs anything the release contains.
- **Update:** `latch update` installs the newest release, `latch update TAG`
  a given one (also how to go back), and `latch update --check` only says
  whether there's a newer one. The owner runs it, and so does an agent on its
  own computer, where latch is installed alone (`--client-only`) and no host
  runs from that latch; elsewhere an agent gets `owner_only`.
  - It checks the release's `SHA256SUMS` against Latch's release signing
    key, and on a Mac that Plow signed the app.
  - The state folder stays as it is, and the browser engines are brought up
    to date. If an earlier update stopped before a helper was installed, it
    installs it again.
  - A host started with `latch serve -d` restarts on the new version with
    the same flags. A host running in a terminal stops, and you're told to
    start it again.
  - A running host looks for a new release once a day, and shows it in
    `latch status --local` and on its dashboard. Nothing installs on its
    own. `LATCH_NO_UPDATE_CHECK=1` turns the looking off.
- **Log out:** `latch logout` stops this machine's host, removes the machine
  from your Plow account and deletes its Plow token. It keeps the machine's
  key, its name, the vault, the audit log, rules and settings, so after
  the next `latch login` it is the same machine, under the same name, to
  every agent. An agent's logout leaves Plow alone.
  - It first says what it deletes and keeps, with the state folder's full
    path and the vault's items by kind, and asks. With no one at a
    terminal it refuses unless given `--yes`; `--dry-run` only says.
  - `latch logout --purge` deletes every secret and the whole state folder;
    at a terminal you type `delete` to confirm.
  - If Plow can't be reached, the local delete still happens. Remove the
    machine from another one with `latch devices remove NAME`.

## 9. Not supported

| What | Status |
|---|---|
| Windows; a host with no sandbox | Not supported |
| Sending texts or iMessages, changing contacts, reading the Messages database directly | Left out on purpose |
| Scripting other apps (AppleScript, System Events) | Left out on purpose |
| Google beyond Gmail and Calendar | Not supported |
| Messages and Contacts on a Linux host | Not supported (`unsupported_platform`) |
| Browser handoff on a Linux host or with `--headless-browsers` | Not supported |
| An agent's login running a host | Not supported: it saves no Plow token, so `latch serve` stops with `not_logged_in` |
| Turning connectors on or off from another machine | Not supported |
| Another machine's dashboard or logs | Not supported |
| Exec stdin over 16 KiB, or stdin that isn't UTF-8 text | Refused (`stdin_not_judgeable`, exit 2) |
| Browsing anything but web addresses, or local addresses without `host.json` | Refused |
| Per-agent permission lists | None, by design: write rules for that agent, or use `--deny-exec=AGENT_UID` |
| A state folder in a format this Latch doesn't read | Refused (`state_format`); there's no migration |

## 10. Where a host runs

A host always runs in a place you trust:

| | What it is |
|---|---|
| **Your Mac** | `latch serve` on your own Mac (section 3). |
| **A Linux machine** | `latch serve` on a Linux machine of your own, a Raspberry Pi included, or in [Docker](../latch/DOCKER.md). |
| **A cloud machine from Plow** | Plow can set up a host in the cloud for your account. It is a host like any other: its gatekeeper judges what your agents do there. |

Whichever you choose, you manage it from the `latch` command on your own
machine: on the host itself, or from another machine with `--host`. For
example, `latch vault import FILE --host NAME` brings a password manager's
export into that host's vault.
