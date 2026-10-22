# Getting started with latch

latch lets you, and agents you allow, reach the files, programs and browser
on your own machines. You log each machine in with your Plow account. One
machine *serves* a folder. Your other machines and your agents *call* it.

This guide takes you from nothing to a Mac serving a folder and a second
machine calling it, in about 10 minutes.

Every command prints one JSON object, except at a terminal, where
`login` and `status --pretty` print for a person.

Exit codes: 0 ok, 1 failed, 2 usage, 3 host unreachable, 4 denied,
5 connection lost after sending.

## 1. Install

You need the GitHub CLI, logged in to an account that can read
`plow-pbc/latch-cli` (`gh auth login`). This installs the newest published
release:

```sh
gh release download --repo plow-pbc/latch-cli --pattern install.sh && sh install.sh
```

`sh install.sh TAG` installs that release instead (tags look like
`v0.1.0-1327`). The installer picks the archive for your
machine (Apple silicon or Intel Mac, Linux x86_64 or arm64, including a
Raspberry Pi), checks it against the release's `SHA256SUMS`, and installs
it. Options after the tag (or alone) go to the installer: `--bin-dir DIR`,
`--state-dir DIR`, `--browser chrome|camoufox` (repeat it for both) and
`--no-browser`.

**What it puts where:**

| What | Where |
|---|---|
| The `latch` binary | `~/.local/bin/latch`. If that folder isn't on your `PATH`, the installer says so; add it. On macOS it links into `~/Applications/Plow Latch.app`. |
| Your machine's state: its key, Plow login, config, audit log | `~/.latch` (made by the installer, filled in by `latch login`) |
| gog, the Google CLI a host runs for `latch google` | `~/.latch/plugins/gog/<version>/` |
| Chrome for Testing, the browser a host runs for `latch browser` (macOS, Linux arm64; about 200 MB), unless `--no-browser` | `~/.latch/plugins/cft/<version>/` |
| Camoufox, with `--browser camoufox` (macOS; about 1 GB) | `~/.latch/plugins/camoufox/<version>/` |

`latch browser setup --engine chrome|camoufox` installs an engine later, on
the host.

`--state-dir DIR` on the installer puts the plugins in another state folder
instead; pass the same `--state-dir DIR` to every `latch` command then.

**Update:** run the same command again, adding `--clobber` to `gh release
download` so it replaces the `install.sh` already there. The installer replaces
the binary in place and skips anything already installed at the pinned
version. Running it twice changes nothing. A host that `latch serve` or
`latch serve -d` started is stopped and started again with the new latch,
with the same folders and flags.

Check it:

```sh
latch --version
# latch 0.1.0 (1327): commit …, built …, standalone tag v0.1.0-1327, protocol plow/latch/1
```

**Linux:** a host sandboxes what it runs with bubblewrap. The installer
installs it (and, on Ubuntu 24.04 and later, loads the AppArmor profile that
lets it work), asking for `sudo` once, only if something is missing. A Pi
also gets Xvfb and unzip for Chrome.

## 2. Log in

```sh
latch login --api-url https://api.plow.co
```

`--api-url` is needed on the first login only; latch saves it. The machine is
named after its hostname; `--name NAME` picks another. Names are unique in your
account: if another of your machines holds the name, the login stops after
the text with `conflict` and never replaces it. Run it again with another
`--name`, and `--api-url` again (no second text needed). Or, from another of
your machines, free the name with `latch devices remove NAME --yes`, which
signs that machine out. `latch devices` lists your machines.

**What you see:** a QR code, then

```
sms:+1…?&body=GHCIA
Text GHCIA to +1 555-…
```

Text that code to that number from the phone your Plow account uses (or scan
the QR with the phone's camera, which opens Messages with the text ready).
latch checks every 3 seconds and then prints `Logged in as <name>.`

Check it:

```sh
latch status            # JSON: this peer, your Plow URL, and the hosts it can reach
latch status --pretty   # the same for a person, with key art to compare between machines
latch status --local    # only this machine: state folder, login, serve, shares. Works offline
```

## 3. Choose what to share, then serve

Pick a folder for callers to work in, save it, and serve:

```sh
latch shares set ~/code/my-project
latch shares          # {"shares":["/Users/you/code/my-project"]}
latch serve
```

`latch serve` runs in the foreground with a live dashboard; each call shows up
as a row. Ctrl-C stops it. Leave it running in its own terminal. (`latch serve
FOLDER…` shares those folders this time instead of the saved ones.)

What a caller from one of your machines can reach:

- **Files:** `file get` reads what exec can: your home folder and the
  system's own files (`latch file get --host mac /etc/hosts` works). `file
  put` writes anywhere in your home folder. Neither touches latch's own
  state or, on macOS, `~/Library` (its Caches excepted); anything else is
  refused (`bad_path`, exit 4). A put replaces the file only once all of it
  has arrived, and keeps the old file's permissions.
- `exec` starts in the first shared folder.

> **Never share your home folder (or `/`).** latch does not stop you: `latch
> shares set ~` is accepted without a warning. Then every caller, including
> agents you allow, can change or delete anything in it: your SSH keys,
> shell profile, browser profiles, other credentials. Share a project folder.

`latch shares set` replaces the saved list. A running `latch serve` keeps its
folders until you restart it.

## 4. Call the host from another of your machines

On the second machine: install (step 1) and log in (step 2) with **the same
phone**, so it joins the same Plow account. Give it its own name. Below, the
host is `my-mac`; use the name your host logged in with.

```sh
latch login --api-url https://api.plow.co --name my-laptop
latch status    # "hosts":[{"name":"my-mac","online":true,…}]
```

To try it on the serving machine, use the same state folder as `latch serve`;
no second login is needed. Leave out `--host` to use its private local
receiver, or name this host explicitly. If you started serve with
`--state-dir DIR`, pass that same option to each command:

```sh
latch --state-dir DIR exec -- ls -la
```

Your own machines are you, and the host's gatekeeper judges their calls
too: `exec`, `file`, `google`, `messages` and `contacts` run only as your own
request within your rules on that host, so they need the decision model (see
6). The browser's judged actions need it too: opening a new site, plans, vault
fills, scripts, uploads and downloads (see [the browser gatekeeper](../latch/BROWSER.md#the-gatekeeper)).
Your settings (`rules`, `vault`, `shares`) are never judged. Set
`LATCH_HOST=my-mac` to leave out `--host`.

**Run a program.** No shell is involved unless you run one. The JSON has
`stdout`, `stderr` and `exit_code`:

```sh
latch exec --host my-mac -- ls -la
latch exec --host my-mac -- sh -c 'pwd; uname -s'
# {"exit_code":0,"stdout":"/Users/you/code/my-project\nDarwin\n",…}
```

**Copy a file.** Paths on the host are in its home folder: `'~/…'` (quoted,
so your shell leaves the `~` alone) or relative to home:

```sh
latch file get --host my-mac '~/hello.txt'             # to ./hello.txt
echo "note from my laptop" > notes.txt
latch file put --host my-mac notes.txt '~/Documents'   # as ~/Documents/notes.txt
```

**Drive a browser on the host** (needs a build with the browser, on both
machines; see 1). `open` answers a session id that every
later command needs; `LATCH_BROWSER_SESSION` carries it. On a Mac the window
opens behind yours and never takes focus.

```sh
export LATCH_HOST=my-mac
latch browser open https://example.com > open.json
export LATCH_BROWSER_SESSION=$(jq -r .session open.json)
latch browser text                          # the page's text
latch browser screenshot page.png  # a PNG on this machine
latch browser observe --all                 # elements with refs (e1, e2, …); without --all, on-screen ones only
latch browser act click e1                  # needs the decision model (see 6)
latch browser close
```

See `latch browser --help` for `do`, `fill`, tabs and handoff, and
[latch/BROWSER.md](../latch/BROWSER.md).

## 5. Give an agent access, the right way

An agent gets **its own login, with its own Plow agent credential, in its own
state folder**. The host then judges each of the agent's calls (its
*gatekeeper*) against what you asked that agent in your Plow chat with it and
against your rules on that host.

> **Never copy a device login (a machine's state folder, or its
> `plow-token`) to an agent.** That login is you: the host sees one of your
> own machines, and the gatekeeper judges the call as your own request, never
> against what you asked that agent. A copied laptop state runs `exec` on your
> host as if you had asked for it.

**An agent already running on Plow** has its credential and Plow's address
in its environment (`$PLOW_AGENT_TOKEN`, the file `$PLOW_TOKEN_FILE` or
`/config/secrets/plow-api-token`, and `$PLOW_API_BASE`, the proxy that adds the
credential). Run, in that environment as it is:

```sh
latch --state-dir ~/latch-agent login
# {"peer":{"kind":"agent","name":"my-agent",…},"key_created":true}
latch --state-dir ~/latch-agent status    # "kind":"agent", and your hosts
```

Don't add `--api-url` or a token file: the agent's own environment already
says where Plow is and who it is. Keep that environment for every command the
agent runs; latch does not copy the credential into the state folder.

**A new agent you run yourself** needs a *self-hosted* agent credential
first, made on one of your free lines.

1. Create it. This needs your own Plow login in the shell, made the way
   Plow's account page makes one: Plow texts you a code, and the code buys a
   login token. (latch keeps its own login where only latch reads it: in the
   Keychain on a signed Mac.) The agent's token is shown once; this writes it
   to a 0600 file and prints only the agent's uid and name:

   ```sh
   printf 'Your phone, as +15551234567: '; read -r P
   curl -s -X POST https://api.plow.co/v1/auth/otp/request \
     -H 'content-type: application/json' -d "{\"phone\":\"$P\"}" >/dev/null
   printf 'The code Plow texted you: '; read -r C
   T=$(curl -s -X POST https://api.plow.co/v1/auth/otp/verify \
     -H 'content-type: application/json' -d "{\"phone\":\"$P\",\"code\":\"$C\"}" | jq -r .token)
   curl -s https://api.plow.co/v1/lines -H "Authorization: Bearer $T" | jq -c '.data[] | {uid, available}'
   umask 077
   curl -s -X POST https://api.plow.co/v1/agents -H "Authorization: Bearer $T" \
     -H 'content-type: application/json' \
     -d '{"line_uid":"LINE_UID","provider":"self_hosted","name":"my-agent"}' \
     | tee >(jq -r .token > ~/my-agent.token) | jq -c '.agent | {uid, name}'
   ```

   Treat `~/my-agent.token` like a password: it carries the agent's full Plow
   scopes. To revoke it, with `$T` from above (or a new one made the same
   way): `curl -X DELETE https://api.plow.co/v1/agents/AGENT_UID -H
   "Authorization: Bearer $T"`. `$T` is your own login: `unset T` when done.

2. Log it in, in its own state folder, with the token file and Plow's public
   address:

   ```sh
   export PLOW_TOKEN_FILE=~/my-agent.token
   latch --state-dir ~/latch-agent login --api-url https://api.plow.co
   # {"peer":{"kind":"agent","name":"my-agent",…},"key_created":true}
   latch --state-dir ~/latch-agent status    # "kind":"agent", and your hosts
   ```

   The agent registers under its Plow name. Keep `PLOW_TOKEN_FILE` set for
   every command the agent runs; latch does not copy the credential into the
   state folder.

**What the agent gets.** With no chat and no rules, every call is refused:

```sh
latch --state-dir ~/latch-agent exec --host my-mac -- uname -a
# exit 4 {"error":{"code":"no_intent","message":"The owner has given this agent no instructions: no Plow chat and no rules on this host."}}
```

Tell it what it may do, either in your Plow chat with the agent ("yes, run
uname -a on my-mac"), or as standing rules on the host. Rules are owner only,
run on the host itself or from one of your machines with `--host`:

```sh
latch rules edit                          # opens $EDITOR; on the host itself
latch rules edit --host my-mac            # from another of your machines
latch rules edit --from my-agent          # rules for one agent only
latch rules show
```

Rules are judged by a model, not guaranteed. For a hard stop, serve with
`latch serve --deny-exec` (every agent) or `--deny-exec=AGENT_UID`.

What the agent sees when refused (exit 4, `gatekeeper_denied`):
`not_covered` means ask you in chat, naming exactly what it will do, then
retry once you agree. `owner_said_no` and `exec_denied` mean no.

## 6. Troubleshooting

**`host_offline` (exit 3): "host … is offline".** Plow lists the host
offline: it isn't serving. Start `latch serve` there. Nothing was sent.

**`host_unreachable` (exit 3): "could not connect to host …; nothing was
sent".**
- Plow still lists the host online, but it doesn't answer: it may have
  stopped without saying so. Plow lists it offline once its heartbeat runs
  out; until then a call takes about 18 seconds to fail.
- Several calls at once from the same machine (or the same agent) can fail
  this way even though the host is up: in a test, 3 of 8 parallel `exec`s
  failed. Nothing was sent, so it's safe to retry; run calls from one
  identity one at a time where you can.
- `outcome_unknown` (exit 5) is different: the connection dropped *after* the
  request was sent. Check whether it happened before you resend.

**`gatekeeper_unavailable` (exit 3).** The host's gatekeeper couldn't reach
Plow or its decision model, whether an agent or one of your own machines
made the call. The action needing that judgment was not run. Retrying starts
the call again; for a browser plan, inspect its `steps` and `trail` first,
since earlier actions may already have happened.

**`model_unavailable` (exit 1, browser).** A browser step needed the decision
model (a click's risk, a goal in words, a sentence check) and couldn't get it,
and the plan stopped there. It never undoes or replays anything, and actions
may already have gone out:
- earlier steps in the same `do` may have clicked, typed or submitted;
- the failed step itself may have acted first: a goal in words runs several
  actions before the call that failed, and a step's check (` -> …`) runs
  after its action was sent.

Before retrying, read `steps` (each step's `outcome`) and `trail` (each action
sent, with its `result`) in the answer, including the failed step's own
entries, and retry only what didn't happen.

For either one, the message says why the model couldn't answer:
- "out of credits for the decision model": the **host's** Plow account is out
  of credits. Every retry fails the same way until the host's owner adds
  credits.
- "Plow's decision route answered HTTP 503; retry later": retry later.

Your own machines need the model for every judged call (`exec`, `file`,
`google`, `messages`, `contacts`) and the browser's judged actions, including
`browser open` and `do`. Browser goals and sentence checks also need the
model. Page reads (`text`, `screenshot`) in an existing session and your
settings (`rules`, `vault`, `shares`) work without it.

**`state_format` (exit 1): "… is in state format 2, set up by latch …; this
latch … reads only format 1".** That state folder was set up by a newer latch
(update this one) or an older one (use that one). Nothing was read or
changed. Don't edit `latch.json` by hand.

**`not_configured` (exit 2) on the first login: "first login needs
--api-url"**. Pass `--api-url https://api.plow.co` (step 2).

**`not_logged_in` (exit 2).** Run `latch login`, with the same `--state-dir`
if you use one.
