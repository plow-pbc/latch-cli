# Getting started with Latch

This guide takes about 10 minutes. You will:

1. Install Latch on the machine your agents will use.
2. Log it in to Plow and start its host.
3. Call the host from another of your machines.
4. Give an agent a login of its own.

Below, the host is `my-mac`, your second machine is `my-laptop` and the agent
is `my-agent`. Use your own names.

**How commands answer.** Each command prints one JSON object on stdout.
When it fails, it prints one `{"error": …}` line on stderr instead.
`--pretty` indents the JSON. `latch` alone, `latch setup`, `latch serve` and
`latch dashboard` are for a person at a terminal.

| Exit code | Meaning |
|---|---|
| 0 | Done. |
| 1 | Failed. Read the error. |
| 2 | The command is wrong, or the host is unknown or ambiguous. |
| 3 | The host could not be reached, or could not judge the call. |
| 4 | Refused. |
| 5 | The request was sent, but the answer was lost. Check what happened before you send it again. |

## 1. Install

```sh
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh
```

The installer downloads the newest release for your machine, checks that
it is signed with Latch's release key, as `latch update` does, and on a Mac
that Plow signed the app, then installs it. You need `curl`, `tar` and a
SHA-256 tool, which most systems have, and on Linux OpenSSL 3.

| What | Where |
|---|---|
| The `latch` command | `~/.local/bin/latch` |
| The Plow Latch app (Mac only) | `/Applications/Plow Latch.app`, or `~/Applications` if you can't write to `/Applications`. On a Mac, `~/.local/bin/latch` is a link into the app. |
| Latch's state folder: its key, Plow login, settings, vault and audit log | `~/.latch` |
| Chrome for Testing, the browser a host drives for agents | `~/.latch/plugins/cft/` |
| The tool a host runs for Gmail and Calendar | `~/.latch/plugins/gog/` |

**PATH.** If `~/.local/bin` isn't on your `PATH`, the installer adds it in
your shell's login profile: `~/.zprofile` for zsh, or `~/.bashrc` and your
login profile for bash on Linux. It also prints the line that fixes the
terminal you are in:

```text
install-binary.sh: added /Users/you/.local/bin to your PATH in /Users/you/.zprofile
install-binary.sh: to run latch in this terminal now: export PATH="$HOME/.local/bin:$PATH"
```

For any other shell, it prints the line to add yourself.

**On a Mac.** What happens next depends on your macOS version:

| macOS | What happens |
|---|---|
| 26 or later | The installer opens the Plow Latch app, which takes you through setup. |
| 15 to 25 | Setup runs in the terminal. The app's window needs macOS 26. |
| 13 or 14 | Setup runs in the terminal. Google doesn't work: it needs macOS 15. |
| 12 or earlier | Not supported. The installer stops and installs nothing. |

The installer never asks for your password on a Mac.

**On Linux.** A host runs its work in a sandbox, which needs bubblewrap and
user namespaces. If a package is missing, the installer installs it with
`apt-get`, `dnf`, `apk` or `pacman`, and asks for your password (`sudo`)
once. On Ubuntu 24.04 and later it also loads the AppArmor profile that lets
bubblewrap work. For Docker, see [DOCKER.md](../latch/DOCKER.md).

**Options.** Put them after `sh install.sh`:

| Option | What it does |
|---|---|
| `TAG` | Installs that release instead of the newest, for example `v0.1.0-151`. Put it first. |
| `--client-only` | Installs `latch` for a machine that only calls hosts, such as an agent's: no browser, no Google tool, no Messages reader and no Linux sandbox packages. It never asks for `sudo` and runs no setup. At a terminal, on a machine holding an agent's Plow credential, it runs `latch login` once at the end, unless this state folder is logged in already. |
| `--no-login` | With `--client-only` at a terminal, doesn't log in at the end. |
| `--bin-dir FOLDER` | Puts `latch` in `FOLDER`. |
| `--state-dir FOLDER` | Uses `FOLDER` as the state folder. Then pass the same `--state-dir FOLDER` to every `latch` command, or set `LATCH_STATE_DIR=FOLDER`. |
| `--browser chrome`, `--browser camoufox` | Chooses the browser to install. Repeat it to install both. |
| `--no-browser` | Installs no browser. `latch browser setup --engine chrome` adds one later. |
| `--non-interactive` | Doesn't start setup at the end, and leaves your shell profile alone. |
| `--no-modify-path` | Leaves your shell profile alone. |

**Check it worked:**

```sh
latch --version
```

```json
{"version":"0.1.0 (151): commit …, protocol plow/latch/4"}
```

## 2. Log in and start the host

A first install starts setup. To run it yourself:

```sh
latch setup
```

Setup takes these steps. Each one shows the `latch` command it runs, and
each is skipped once done, so you can run setup again at any time:

1. **Log in** to Plow (`latch login`).
2. **Connect Google** (`latch plugins enable google`). If your Plow account
   has no Google account yet, it shows a link and a QR code to sign in.
3. **On a Mac, turn on Messages and Contacts.** These need Full Disk Access
   for the Plow Latch app.
4. **Start the host** (`latch serve -d`), if you say yes.

`latch setup --status` lists the same steps as JSON and changes nothing.

**Logging in by text.** `latch login` shows a QR code and a line like this:

```text
Text GHCIA to +1 555-000-0100
```

Text that code from the phone your Plow account uses, or scan the QR code
with the phone's camera. Latch waits for the text, then prints
`Logged in as my-mac.`

The machine is named after its hostname. `latch login --name NAME` picks
another name. Names are unique in your account. If another of your machines
has the name, the login asks whether to replace that machine or which name to
use instead. `latch devices` lists your machines. Run on a machine that is
logged in already, `latch login` changes nothing and says as whom;
`latch login --again` logs in anew.

**See this machine at a glance:**

```sh
latch
```

It shows your account, whether this machine is hosting, your hosts, its
connectors and one next step. `latch status` gives the same as JSON, and
`latch status --local` only this machine, without asking Plow.

**Start the host** if setup didn't:

```sh
latch serve
```

`latch serve` starts the host and shows its dashboard, with a row for each
call. `q` closes the dashboard and leaves the host running. Ctrl-C stops the
host. To start it in the background instead, run `latch serve -d`. To stop
it, run `latch stop`.

## 3. Call the host from another machine

On your second machine, install Latch and log in with **the same phone**, so
it joins the same Plow account. Give it its own name:

```sh
latch login --name my-laptop
latch status
```

`latch status` lists your hosts, with `"online":true` for `my-mac`.

**Run a program.** No shell runs unless you name one. The answer has
`stdout`, `stderr` and `exit_code`:

```sh
latch exec --host my-mac -- ls -la
latch exec --host my-mac --write '~/code/my-project' -- sh -c 'pwd; uname -s'
```

```json
{"exit_code":0,"stdout":"/Users/you/code/my-project\nDarwin\n",…}
```

The program can read your home folder and the system's own files. It can
write only to the folders you name with `--write`, its own private temp
folder, and tool caches that already exist, such as `~/.cache` and `~/.npm`.
It has no network unless you add `--network`.

**Copy a file.** Paths on the host are in its home folder. Quote a leading
`~` so your own shell leaves it alone:

```sh
latch file get --host my-mac '~/hello.txt'
latch file put --host my-mac notes.txt '~/Documents'
```

The first saves `./hello.txt`. The second writes `~/Documents/notes.txt` on
the host. Neither reaches Latch's state folder or, on a Mac, `~/Library`:
those are refused with `bad_path`.

**Drive a browser on the host.** `browser open` answers with a session id,
which later commands read from `LATCH_BROWSER_SESSION`:

```sh
export LATCH_HOST=my-mac
latch browser open https://example.com > open.json
export LATCH_BROWSER_SESSION=$(jq -r .session open.json)
latch browser text
latch browser screenshot page.png
latch browser observe --all
latch browser act click e1
latch browser close
```

`LATCH_HOST` saves typing `--host` each time. `observe` lists the page's
elements as `e1`, `e2` and so on. See [BROWSER.md](../latch/BROWSER.md).

**On the host itself,** leave out `--host`, and use the same state folder as
`latch serve`. Calls such as `exec` then go to the running host directly.
`rules` and `vault` work there even when the host is stopped.

**Your own calls are judged too.** The gatekeeper checks `exec`, `file`,
`google`, `messages`, `contacts` and the browser's risky steps against your
rules on that host. Your settings commands (`rules`, `vault`) are never
judged.

## 4. Give an agent its own login

An agent always logs in with **its own** Plow credential, in its own state
folder. The host then judges each of its calls as that agent's.

> **Never copy your own login to an agent.** Your login is a machine's state
> folder, or its Plow token. A host treats that login as you, so it would
> judge the agent's calls as your own requests.

**An agent that runs on Plow** has its credential and Plow's address in its
environment (`PLOW_AGENT_TOKEN`, `PLOW_TOKEN_FILE` or `PLOW_API_BASE`). If
its machine has no Latch yet, install it with
`sh install.sh --client-only` in that environment: at a terminal, it logs
in at the end. Then, in that same environment (`latch login` logs in when
the install didn't, and otherwise says it is logged in already):

```sh
latch login
latch status
```

`latch status` shows `"kind":"agent"` and your hosts. Keep that environment
for every command the agent runs: Latch doesn't copy the credential into its
state folder.

**An agent you run yourself** needs a free phone line in your account. From
one of your own machines:

```sh
latch agents lines
latch agents new --own --name my-agent --line LINE --token-file my-agent.token
```

- `LINE` is a line's number or uid from `latch agents lines`.
- `my-agent.token` is a new file. Latch makes it private (mode 600) and
  never overwrites a file.

Treat the file like a password. Give it to the agent. Its program logs in
with it, in a state folder of its own:

```sh
export PLOW_TOKEN_FILE=~/my-agent.token
latch --state-dir ~/latch-agent login
```

Keep `PLOW_TOKEN_FILE` set for every command the agent runs.

To revoke the agent, run `latch agents remove my-agent`. It asks you to
confirm (`--yes` skips the question). The agent's credential stops working at
once and its line is freed.

**What an agent may do.** Your agents can look things up in your calendar,
email, messages and files, and create or edit ordinary files, without asking
first. What they read goes back to the agent. Riskier actions need your
permission, in your rules or in your Plow chat with the agent. These include
sending, buying, deleting, changing calendars or shared systems, giving a
program internet access, using secrets, and sending data elsewhere.

**Rules** are plain text in your own words. There is one text for every
agent, and one for each agent. Only you can read or change them:

```sh
latch rules edit                     # on the host itself
latch rules edit --host my-mac       # from another of your machines
latch rules edit --agent my-agent    # rules for one agent only
latch rules show
```

`latch rules set TEXT` sets them without an editor. Your rules stay in force
when you ask in chat, and they can restrict everyday work too, for example
"Do not read or search this Mac's Messages."

A model judges the rules, so they are not a guarantee. For a hard stop, start
the host with `latch serve --deny-exec`: no agent can run programs there.
`--deny-exec=AGENT_UID` stops one agent.

**When an agent is refused** (exit 4, `gatekeeper_denied`):

- `not_covered`: the agent asks you in chat, saying exactly what it will do,
  and tries again once you agree.
- `owner_said_no` or `exec_denied`: the answer is no.

## 5. Update

```sh
latch update
```

This installs the newest release. `latch update --check` only says whether
there is a newer one. `latch update TAG` installs a given release, which is
also how you go back to an older one.

Latch checks the release's signature first, and on a Mac that Plow signed
the app. A host started with `latch serve -d` restarts on the new version.
A host running in a terminal stops, and the update tells you to start it
again. A running host checks for a new release once a day and shows it in
`latch` and on its dashboard. Nothing installs on its own.

An agent runs `latch update` on its own computer too, where latch is
installed alone (`install.sh --client-only`) and no host runs. An agent's
login can't update a host, or a computer where a host runs: the owner does.

## 6. Troubleshooting

| You see | What it means | What to do |
|---|---|---|
| `command not found: latch` | `~/.local/bin` isn't on your `PATH` in this terminal. | Open a new terminal, or run the `export PATH=…` line the installer printed. |
| `host_offline` (exit 3) | Plow doesn't list the host as online. Nothing was sent. | Start `latch serve` on the host. |
| `host_unreachable` (exit 3) | No connection to the host within 15 seconds, or the connection failed. Nothing was sent. | Try again. A host that stopped without telling Plow stays listed as online for up to 25 minutes. |
| `outcome_unknown` (exit 5) | The request was sent, but the answer was lost. | Check whether it happened before you send it again. |
| `serve_required` (exit 3) | This machine's host isn't running. | Start `latch serve` with the same state folder. |
| `host_required` (exit 2) | You have several hosts and didn't name one. | Add `--host NAME`, or set `LATCH_HOST`. |
| `gatekeeper_unavailable` (exit 3) | The host couldn't reach Plow or its decision model, so the call didn't run. | Try again later. If the message says the host's Plow account is out of credits, the host's owner must add credits. For a browser plan, read its `steps` first: earlier steps may have run. |
| `model_unavailable` (exit 1) | A browser step needed the decision model and couldn't get it. Earlier steps may have acted. | Read `steps` and `trail` in the answer, and retry only what didn't happen. |
| `not_logged_in` (exit 2) | This state folder isn't logged in. | Run `latch login`, with the same `--state-dir` if you use one. |
| `not_configured` (exit 2) | A Latch built from source doesn't know which Plow to use. | Pass `--api-url https://api.plow.co` to `latch login`. |
| `state_format` (exit 1) | The state folder belongs to another version of Latch, or holds files Latch didn't put there. Nothing was read or changed. | Update Latch, or use the version that made it. For a folder with other files, run the command in the error's `next`, which moves them aside, or use another folder. |

In `~/.latch`, Latch moves files it didn't put there into
`~/.latch/leftovers/DATE` by itself, and says so.

**Connection problems.** Set `LATCH_TRACE=iroh=debug,latch=info` on the
calling machine, the host or both. A command writes its trace to `trace.log`
in its state folder. The host writes it to its log (`latch logs`).
