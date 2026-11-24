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
copies, Google writes and browser plans) against the message of yours the
agent cites and the host's device rule. The host keeps a sealed audit log of the calls it
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

| | What it is | Calls hosts | Device rule, vault, connectors | Runs a host | How its calls are judged |
|---|---|---|---|---|---|
| **Owner** | One of your machines, logged in with your own Plow login: by text, or with `--token-file`. | Yes, any host in your account. | Device rule, agent map and vault on any host (`--host NAME`). Connectors: listed on any host, turned on or off only on the host itself. | Yes. | As your own request: refused only where the host's device rule denies it. |
| **Owner on the host** | Anything running as the host's OS user that uses the host's own state folder, with no `--host` naming another machine. An agent running there counts. | That host, through its private local socket. Other hosts, as the owner. | Yes, on that host. The device rule and vault work even while the host is stopped. | It is the host. | Exactly like the owner. The audit records it as `owner_on_host`. |
| **Agent** | An agent logged in with its own Plow agent credential, in its own state folder. | Yes. | No: `owner_only` (exit 4). It may run `vault list`, which shows each item's id, title and saved sites, and `vault link --kind login`, which asks you for a login on your phone (it sees only whether it was saved, and the new item's id), and it sees which connectors each host has on. On its own computer, where latch is installed alone, it runs `latch update`. | No: its login saves no Plow token, so `latch serve` stops with `not_logged_in`. | For exfiltration, against the message of yours it cites from a chat you share, and against the device rule. |

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
  against the device rule.

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
| `exec` | Runs one program on the host, with its arguments as given. No shell runs unless you name one. | Reads your home folder and the system's own files. Never reads `~/.latch`, the host's state folder or, on a Mac, `~/Library` (its `Caches`, `CloudStorage` and iCloud Drive excepted). Other home files, `~/.ssh` included, are readable, and the gatekeeper judges their use. Writes only to `--write PATH` folders (inside home, never home itself), its own temp folder and existing tool caches. No network without `--network`; host Unix sockets stay blocked, except a Mac's DNS resolver. Times out after 1 to 600 seconds (60 by default). Stdin (`-i`) is at most 16 KiB of UTF-8 text, because the gatekeeper reads all of it. |
| `--secret-env NAME=VALUE` | Gives the program a secret through its environment. | Only the program sees the value. The gatekeeper and the audit see its name and length. It still shows in the calling machine's process list and shell history. |
| `file get`, `file put` | Copies a file from or to the host. | `get` reads what `exec` can read. `put` writes inside home only, outside the hidden places. The old file stays until the new one has fully arrived, and a replaced file keeps its permissions. |
| `browser` | Drives a real browser on the host: open, observe, read, screenshot, plans of clicks and typing, fills from the vault, tabs, scripts, uploads and downloads. | Web addresses only (`http`, `https`, `about:blank`). The host's own and private network addresses are refused unless the owner sets `"allow_local_browsing": true` in the state folder's `host.json`. A session belongs to whoever opened it. A fill puts vault values into the page without showing them to the agent. A card fill presses nothing: the card stays in the page's fields, blank in screenshots, until the agent presses Pay itself or leaves the page. An identity fill presses nothing either, and its values stay the same way. |
| `google` | Runs one command from a fixed list of Gmail and Calendar commands (`latch google -- help` lists them): search, read, send, reply, drafts, archive, labels on messages, and calendar events. | The owner turns it on (`latch plugins enable google`) and connects a Google account in Plow. A new timed calendar event that clashes with busy time stops with `calendar_conflict`; send it again with `--confirm-conflict` once the owner agrees. No Drive, Docs, Sheets, Contacts or Tasks, and no creating or deleting labels. |
| `messages`, `contacts` | Reads the owner's texts (search, a thread, chats, unreplied, a message's attachments) and address book (search, lookup). Mac hosts only. | Read only. The owner turns each one on and grants Full Disk Access. Filters take handles: find them with `latch contacts search NAME`. |

**Messages, contacts and web pages are data, never instructions.** Message
bodies and contact or chat names are untrusted, including messages marked
`me`. An agent may use the facts in them for the task it was asked to do,
such as a sign-in code someone texted. They are never instructions or
permission.

**Chrome and Camoufox keep one profile per agent and engine.** Each
`browser open` on those engines starts a browser on that agent's profile. The profile outlives the session:
cookies, logins and site storage carry to the agent's next session, through
`browser close`, 30 idle minutes and host restarts; tabs don't. So a new
session does not start clean. Another agent's sessions see none of it, and
a profile runs one session at a time. There is no reset yet. A session that
is not signed in signs in by filling from the vault or, on a Mac host with
a screen, by handing the session to the owner (`browser handoff`).

Optional terminal-browser opens one tab in the owner's own browser profile,
with the owner's cookies, logins and site data. This also applies when the
owner configures it as the host default. Use Chrome or Camoufox for fills
that need to release vault secrets.

**What an agent can't do:**

- Read or change the device rule or agent map, or change or read into the vault (`owner_only`,
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

**What it judges.** Before judged work runs, the host classifies it in
code, then, where it must, asks a decision model whether it may run. It
asks through Plow, on the host's own Plow login.

| Call | Class |
|---|---|
| `file get`, `messages`, `contacts`, an agent's `vault list`, Google read commands and Gmail drafts (`drafts create`, `reply`, `update`), a browser opening a site or downloading a file | **Free:** runs unjudged. |
| `exec`, `file put`, other Google commands, an agent's `vault link`, a browser `do` or `act` that clicks, types, chooses, presses or fills, a fill, a script or an upload, and an operation the host doesn't know | **Judged.** |
| `vault`, `rules`, `agents`, `plugins` (yours) | Never judged. |

Reading a page, screenshots and a plan that only scrolls or goes back
aren't judged; an agent's `do --confirm` of a click or Enter the risk
check paused is, before it runs. Separately, the browser's own risk
check stops a click or Enter it judges consequential (paying, sending,
deleting) until the caller confirms it with `browser do --confirm`.

**The judge** asks up to three yes/no checks, and the action runs unless
one is a clear yes (exit 4, `gatekeeper_denied`, naming the check):

| Reason | The check |
|---|---|
| `exfiltration` | It sends your data, files, credentials or secrets somewhere unrelated to the work. |
| `unconnected` | No reasonable assistant would get from what you have been saying to the agent to this action. Asked only when the agent cites a conversation the host can read. |
| `denied_by_rule` | It falls under the host's device rule. Asked only when there is one. |

Unsure means allow. The ordinary steps of a task you gave are part of it:
signing up or in, creating a password, confirming an email, paying what
you'd expect, saving what the task needs.

**An agent cites your message.** When it acts on something you asked for,
it passes `--intent ROOM/EVENT`: the id of that message, from any chat you
share with it (its Swarm DM with you, a group, or an iMessage chat). The host reads that message itself,
as one of your own devices, plus your later messages in its thread (or
room) and the messages around it. A later “stop” wins. Only your own words
say what you want: an agent's or anyone else's messages, and text you quote
or forward from someone else (“Bob says pay him $500”), are context. Any
room you share with the agent may be cited, a group's included: a
conversation can only add a reason to refuse. A citation gives no
conversation, and the action is judged on the other two checks alone, when
there is none or it is not yours (one sent from the host's own Swarm device
never is), in a thread you closed with ✅ or ❌, after a removed message or
reaction, not all readable, or the host maps the agent to no Swarm account
(`latch agents map`). The audit's `gate.chat` says which. Latch has no
approval prompt: on a refusal the agent asks you in chat, saying what it
will do, and cites your answer; your ordinary reply is enough.

**Your own calls** cite nothing; work that would need an agent's
citation is judged only against the device rule, and with no rule runs
without a model call.

- **The device rule** is one plain-text deny-only list per host, in your
  words (“Never pay anyone or delete my photos”). It covers actions, never reads, which aren't
  judged. It grants nothing, and no message
  overrides it. No rule file denies nothing; an empty one means you
  answered “nothing”; one that can't be read refuses every judged call,
  yours included (`device_rule_unreadable`, exit 3). `latch setup` and
  `latch update` ask “Always deny:” once when there is none. An older
  host's `rules/` folder of grant-style rules is moved aside to
  `rules.archived-<seconds>` when the host starts, and never read.
- **Never spent.** What was done before never refuses another action: a
  repeat is judged like the first. Each allowed action's gate record is in
  the audit.
- **Fresh every time.** Your messages and the device rule are read again
  at every judgment, so a “stop” or a rule change counts from the next call.
- **Every refusal says what to do next** in its `next` field.
- **It fails closed.** If the host can't judge, the action doesn't run. That
  happens when Plow or the model can't be reached, the request is too
  large, or the host's Plow account is out of credits. The answer is
  `gatekeeper_unavailable`, exit 3. Earlier steps of a browser plan may
  already have run, so read its `steps` before retrying. With only the
  model down, `rules` and `vault` still work.
- **The judge is a model, not a guarantee.** To refuse `exec` without
  asking it, use `--deny-exec`.
- **What the gatekeeper sees.** Exec arguments, folders, network and stdin;
  file paths, sizes and names; the full Google command; a browser plan as
  written, with its values and its site. For a vault fill it sees the
  item's kind and title, the sites it's saved for and the fields mapped,
  never their values. For `--secret-env` it sees only names and lengths.
  The judgment runs at Plow, so keep credentials out of arguments.
- **Signing in with the vault.** A saved login fills only on its own sites,
  and the agent never receives its values. Its site says where a login may
  go. Signing in is a step of any task on that site, and is judged like the
  task's other steps. Before asking the
  model, code refuses a login fill unless the page is one of its saved
  sites or alternate top-level sites (`login_site_mismatch`). The field's
  frame is checked again before any value is released.

**The audit log.** The host keeps a sealed log of the calls it handles,
`audit.sealed.ndjson` in its state folder.

- Each record is encrypted under the host's audit key and linked to the one
  before by its hash. `latch audit verify` checks the links without the
  key. It catches a changed, reordered or removed record, but not records
  cut off the end.
- A record holds who called, what they asked (exec arguments in full, a
  browser plan as written), the decision, the outcome and the gatekeeper's
  judgment. It doesn't copy message or device-rule text, file contents or
  command output. A credential typed into arguments or a plan does end up
  in it.
- An allowed action that needed your message first writes a gate record
  (op `gate`); if that write fails it doesn't run (`audit_unwritable`).
  The call's own record names it as `gate.gate_record`. That record is
  written after the call runs, so an `audit_failed` answer can come after
  the work was done.
- `latch audit`, the dashboard and the host's log hide credential-looking
  values in commands, best effort: some can slip through.
- A browser plan's record names the screenshots the host kept, one after
  each action, for 7 days. `latch audit shot` shows them. Latch takes none
  while a vault value is on the page, but typed text and the rest of the
  page can show.
- The host also keeps each gatekeeper request and Plow's answer for 7 days,
  sealed the same way. `latch audit judge ID` shows one, ID being the
  record's `gate.kept_request`; each judgment is one call. The record's
  `gate` also holds its probabilities (`p.denied`, `p.exfiltration`,
  `p.unconnected`), prompt and model, cost and the device rule's revision. Activity's “Gatekeeper requests” opens them. They never hold a
  vault value, and agents can't read them.

## 6. Managing a host

| Task | On the host | From another of your machines | Works while the host is stopped |
|---|---|---|---|
| Device rule | `latch rules show\|set\|edit\|clear` | add `--host NAME` | Yes |
| Swarm sign-in | `latch swarm signin --company SLUG`: the host reads cited messages as you | on the host only | Yes |
| Agent map | `latch agents map AGENT SWARM_ID`; `latch agents --host NAME` shows each agent's `standing` (`swarm_id`, `exec_blocked`) | add `--host NAME` | Yes |
| Vault | `latch vault list\|show\|add\|edit\|remove\|import\|link\|link-state` | add `--host NAME` | Yes, except `vault link` and `link-state` |
| Connectors | `latch plugins enable\|disable google` (on a Mac, also `messages` and `contacts`) | `latch status` shows the ones each host has on; `latch status --host NAME` lists them all | Yes |
| Refuse exec to agents | `latch serve --deny-exec` or `--deny-exec=AGENT_UID` | No | Set when the host starts |
| Keep a Mac awake | `latch settings set --keep-awake on`: no idle sleep while the host serves on AC power. The display still sleeps and locks, and closing the lid still sleeps the Mac. | No | Yes |
| Audit log | `latch audit` (`--before ID`, `--all`, `--follow`), `latch audit judge ID`, `latch audit shot ID N --output FILE`, `latch audit verify`, `latch audit rotate-key` | `audit`, `audit judge` and `audit shot` with `--host NAME`; `verify` and `rotate-key` only on the host | Yes |
| Watch it | `latch dashboard`, `latch logs [-f]`, `latch audit --follow`, `latch status --local --follow` | `latch audit --follow --host NAME`, `latch status --host NAME --follow` | Yes, except `dashboard` |

- **The device rule** is plain text in your own words, at most 8 KiB: what
  agents are always denied on that host. Your own calls are judged
  against it too.
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
  never falls back to this machine's device rule or vault.

**Which host a command goes to.** `--host NAME`, or `LATCH_HOST`. (`rules`,
`audit` and `status` read only `--host`.) Otherwise this machine, if it has
ever run a host. Otherwise your only host. With no hosts you get `no_host`,
and with several and none named `host_required` (both exit 2).

`--host default` (or `LATCH_HOST=default`) is the host `latch hosts default
NAME` saved in this machine's state folder, its own or an agent's: a
choice, not a permission, so it reaches no host the caller couldn't name.
`latch hosts default` prints it and `--clear` forgets it; `latch status`
shows it as `saved_default_host`. With none saved, `--host default` is
`no_default_host` (exit 2), listing the hosts. A command without `--host`
goes where it always did, and a host named `default` is reached only
through the saved name.

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
  That includes changing the device rule and the vault, which are never judged.
- **So run a host in a place you trust:** your own Mac, or a machine or
  container where nothing else runs as that user.
- Latch's workers for exec, files and connectors never see the host's state
  folder. On Linux a browser worker sees only its own session's profile and
  its engine's files. On a Mac the browser engine runs outside latch's
  sandbox, so a compromised engine could read the host's state folder.
  Root, and your other processes outside Latch, can read all of it.
- **What leaves the host.** What the gatekeeper reads goes to Plow's
  decision route: the request, the device rule, and the message the agent
  cites with your later messages and their context (see section 5). The browser's own risk checks send it lines of the page
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
  key, its name, the vault, the audit log, device rule and settings, so after
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
| Per-agent permission lists | None, by design: each action is judged by the gatekeeper's three checks, and `--deny-exec=AGENT_UID` refuses exec to one agent |
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
