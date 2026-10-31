# Commands

Every command prints one JSON object. Add `--pretty` to read it yourself.
`latch help COMMAND` lists a command's options, and
`latch help COMMAND --examples` shows how to call it.

| For | Means |
|---|---|
| Host | You, the owner, run it. |
| Agent | Your agent runs it, to ask a host for something. |

Commands marked "Owner only" work from your own computers, never for an
agent.

## Choosing a host

`--host NAME` sends an Agent command to that host, over the encrypted
connection between the two computers. Without it, the command goes to the
host `LATCH_HOST` names; else to this computer, if it hosts; else to your only
host. With several hosts and none named, it stops with `host_required`.
`--host default` is the host `latch hosts default NAME` saved on this
computer, or for this agent.

`vault` and `audit` work on this computer's own host, or on another of your
hosts with `--host NAME`; `rules` work on every host of your account at once. Bare `latch status` reports this computer
and ignores `LATCH_HOST`; `latch status --host NAME` reports another host, for
you only, never for an agent. `latch browser setup` runs only on the computer
you're at: with `--host` it stops with `host_not_supported`.

```sh host
latch vault list --host my-pi
```

```sh agent
latch browser open --host my-mac https://example.com
```

## Set up

| Command | For | What it does, and an example |
|---|---|---|
| `latch setup` | Host | Set this machine up, step by step, at a terminal. |
| `latch login` | Host | Log this machine or agent in to Plow: create its key if missing and register it. |
| `latch logout` | Host | Log this machine out of Plow. |
| `latch status` | Host Agent | Print this machine, its login and account, and the account's hosts. |
| `latch devices` | Host | List this account's machines, online or not, or remove one. |
| `latch devices remove` | Host | Remove a machine and sign out the Plow login it registered with. Owner only.<br>Remove the old Pi without a prompt: `latch devices remove old-pi --yes` |
| `latch hosts default` | Host | The host `--host default` names: print it, save NAME, or clear it. Saved, use it with --host default (or LATCH_HOST=default); commands without --host are unchanged. Kept in this state folder, for this machine or agent alone; it reaches no host you couldn't name yourself.<br>Use mbp when --host default is given: `latch hosts default mbp` |
| `latch update` | Host Agent | Install the newest latch, or the build TAG names. |
| `latch app` | Host | Open the Plow Latch app this latch belongs to (macOS). |
| `latch help` | Host | Help for a command, its examples, or every command as JSON.<br>Help for one command: `latch help browser open` |
| `latch skill` | Agent | Print SKILL.md, which teaches an agent how to use latch. |

## Run a host

| Command | For | What it does, and an example |
|---|---|---|
| `latch serve` | Host | Run this machine as a host. |
| `latch stop` | Host | Stop the host on this state folder; its state is kept. |
| `latch dashboard` | Host | View the running host; Ctrl-C closes only the view. |
| `latch logs` | Host | The host's diagnostics (serve.log).<br>Follow the host's log: `latch logs --follow` |
| `latch settings set` | Host | Save a setting for this machine's host; needs no login, host or restart.<br>Keep this Mac awake while plugged in, for its host: `latch settings set --keep-awake on` |

## Agents

| Command | For | What it does, and an example |
|---|---|---|
| `latch agents` | Host | List your agents, with their lines and Plow status. Owner only. |
| `latch agents show` | Host | Show one agent, by its uid or its exact name. Owner only.<br>One agent, by its name: `latch agents show Scout` |
| `latch agents catalog` | Host | The kinds of cloud agent Plow can run, for `agents new --cloud`. Owner only. |
| `latch agents start-by-text` | Host | Show how to start a cloud agent by texting Plow. Owner only. |
| `latch agents lines` | Host | Plow's phone lines, and which are free for a new agent. Owner only. |
| `latch agents new` | Host | Create an agent on a free phone line. Owner only.<br>An agent you run yourself: `latch agents new --own --token-file scout.token --name Scout --line +15550000002` |
| `latch agents card` | Host | Save the contact card of an agent's line, by the agent's uid or exact name. Owner only.<br>Save an agent's contact card: `latch agents card Willow --output willow.vcf` |
| `latch agents remove` | Host | Remove an agent, by its uid or its exact name. Owner only.<br>Remove an agent without asking: `latch agents remove Scout --yes` |

## Programs and files

| Command | For | What it does, and an example |
|---|---|---|
| `latch exec` | Agent | Run one program on a host, inside its sandbox.<br>Run a shell line on the Pi: `latch exec --host pi -- sh -c 'uname -a; ls'` |
| `latch file get` | Agent | Copy a file from the host to this machine.<br>Fetch a build log from the Pi: `latch file get --host pi build/log.txt ./log.txt` |
| `latch file put` | Agent | Copy a file from this machine to the host.<br>Send a script into ~/scripts on the Pi: `latch file put --host pi ./check.sh scripts/` |

## Browser

| Command | For | What it does, and an example |
|---|---|---|
| `latch browser open` | Agent | Start a new browser session and load URL.<br>Open a page in a new session: `latch browser --host mac open https://shop.example/` |
| `latch browser observe` | Agent | A fresh element list (snapshot), the page's first text, and the tabs.<br>See the page: `latch browser --host mac observe` |
| `latch browser text` | Agent | The page's text.<br>Read the page: `latch browser --host mac text` |
| `latch browser screenshot` | Agent | A PNG of the page, written to a file on this machine.<br>Capture the page: `latch browser --host mac screenshot page.png` |
| `latch browser do` | Agent | Run a plan of up to 10 browser steps, in order.<br>Type, then click and check where it led: `latch browser --host mac do 'type e3 "Ada Lovelace"' 'click e7 -> path: /cart'` |
| `latch browser act` | Agent | Run one browser step on a ref from the last page.<br>Click and check the result: `latch browser --host mac act click e7 --check 'text: Added to cart'` |
| `latch browser handoff` | Agent | Hand this session to the owner, and wait until they hand it back.<br>Ask the owner to solve a CAPTCHA: `latch browser --host mac handoff --reason captcha` |
| `latch browser find` | Agent | Candidate refs by role and exact name; never acts.<br>Find the download button: `latch browser --host mac find --role button --name Download` |
| `latch browser wait` | Agent | Wait for an element's state, text or a path, with a deadline.<br>Wait for a confirmation: `latch browser --host mac wait --text Saved` |
| `latch browser goto` | Agent | Load URL in the session's active tab.<br>Load the next page: `latch browser --host mac goto https://shop.example/cart` |
| `latch browser tab new` | Agent | A new tab, on URL or blank; it becomes the active one.<br>Open a page in a new tab: `latch browser --host mac tab new https://docs.example/` |
| `latch browser tab switch` | Agent | Make tab T (such as t2) the active one.<br>Go back to the first tab: `latch browser --host mac tab switch t1` |
| `latch browser tab close` | Agent | Close tab T, or the active one.<br>Close the second tab: `latch browser --host mac tab close t2` |
| `latch browser dialog` | Agent | Answer the open dialog.<br>Accept a confirm dialog: `latch browser --host mac dialog accept` |
| `latch browser eval` | Agent | Run JavaScript from a file in the page.<br>Run a check script: `latch browser --host mac eval --script-file check.js` |
| `latch browser upload` | Agent | Put a file from this machine into a file input, by its ref.<br>Attach an invoice: `latch browser --host mac upload e4 ./invoice.pdf` |
| `latch browser downloads` | Agent | The session's downloads, as handles (d1, d2, …).<br>List the downloads: `latch browser --host mac downloads` |
| `latch browser download get` | Agent | Write download D (such as d1) to a file here.<br>Bring a report here: `latch browser --host mac download get d1 --output report.csv` |
| `latch browser fill` | Agent | Fill a page's fields from a vault item.<br>Sign in with the login saved for the page's site: `latch browser --host mac fill --field username=e4 --field password=e5 --press e6` |
| `latch browser sessions` | Agent | Your open sessions, without their ids.<br>List your sessions: `latch browser --host mac sessions` |
| `latch browser close` | Agent | End the session, deleting its cookies and logins.<br>End this session: `latch browser --host mac close` |
| `latch browser setup` | Host | Download and verify a browser engine into this machine's state folder. Owner only. |

## Vault

| Command | For | What it does, and an example |
|---|---|---|
| `latch vault list` | Host Agent | The items' metadata, a login's username included (never a secret value).<br>The items saved for a site: `latch vault --host mac list --site crateandbarrel.com` |
| `latch vault show` | Host | One item's metadata and its non-secret values (never a secret one). Owner only.<br>Show an item: `latch vault --host mac show v3` |
| `latch vault add` | Host | Add an item to the vault. Owner only.<br>Add a login with its password as JSON on stdin: `latch vault add --kind login --title Shop --site shop.example --field username=ada --stdin` |
| `latch vault edit` | Host | Change an item's title, sites or fields, at its current revision. Owner only.<br>Change a login's password, given as JSON on stdin: `latch vault edit v1 --revision 2 --stdin` |
| `latch vault link` | Host Agent | Add an item from your phone, through a one-time link.<br>An agent asks its owner for a login, then sends them the url: `latch vault --host mac link --kind login --title Shop --site shop.example --field username=owner@example.com` |
| `latch vault link-state` | Host Agent | Whether a phone link is still pending, saved (with the new item's id) or expired; never a value.<br>Whether the owner has saved the login yet: `latch vault --host mac link-state l3f09a2c1` |
| `latch vault remove` | Host | Remove an item at its current revision. Owner only.<br>Remove an item: `latch vault --host mac remove v3 --revision 1` |
| `latch vault import` | Host | Import another password manager's export. Owner only.<br>Import a 1Password export: `latch vault --host mac import export.1pux` |

## Rules

| Command | For | What it does, and an example |
|---|---|---|
| `latch rules list` | Host | Your rules for every host, read from each host you can reach. Owner only. |
| `latch rules add` | Host | Add a rule: TEXT, the text of --file F (- reads stdin), or a preset's. Owner only.<br>Let agents read mail, never send it, everywhere: `latch rules add 'Agents may search my mail but never send any.'` |
| `latch rules edit` | Host | Change a rule's text, the agents it covers or the hosts it applies on. Owner only.<br>Change a rule's text: `latch rules edit '3f9a…' 'Never send email after 9 PM.'` |
| `latch rules remove` | Host | Remove a rule, on this machine first, then on every host you can reach. Owner only.<br>Remove a rule: `latch rules remove '3f9a…'` |
| `latch rules try` | Host | Preview the gatekeeper's verdict on a request, without running it. Owner only.<br>Would Penny be allowed to build the site? `latch rules try --agent Penny -- exec --write ~/site -- npm run build` |

## Connectors

| Command | For | What it does, and an example |
|---|---|---|
| `latch plugins enable` | Host | Turn a connector on, if its setup succeeds. Owner only.<br>Turn Google on here: `latch plugins enable google` |
| `latch plugins disable` | Host | Turn a connector off. Owner only.<br>Turn Google off here: `latch plugins disable google` |
| `latch google -- COMMAND` | Agent | Run one allowed command on a host's Google account. Calendar: `calendars`, `conflicts`, `create`, `delete`, `event`, `events`, `freebusy`, `move`, `respond`, `search`, `update`. Gmail: `archive`, `attachment`, `drafts create`, `drafts delete`, `drafts forward`, `drafts get`, `drafts list`, `drafts reply`, `drafts reply-all`, `drafts send`, `drafts update`, `forward`, `get`, `labels list`, `mark-read`, `messages modify`, `messages search`, `reply`, `reply-all`, `search`, `send`, `thread get`, `unread`.<br>Search one account's mail: `latch google --host mac --account ada@example.com -- gmail search 'from:grace newer_than:7d'` |
| `latch messages search` | Agent | Search messages for PHRASE, newest first.<br>Find a sign-in code texted in the last ten minutes: `latch messages search --host mac --since 10m code` |
| `latch messages thread` | Agent | One conversation, oldest first, ending with its newest message.<br>The direct conversation with one handle: `latch messages thread --host mac --with +15550000001` |
| `latch messages chats` | Agent | Conversations, most recently active first, with who is in them.<br>Recent group chats: `latch messages chats --host mac --kind group` |
| `latch messages unreplied` | Agent | Direct conversations whose newest message is inbound: candidates, not proof a reply is owed.<br>Unanswered in the last day: `latch messages unreplied --host mac --within 24h` |
| `latch contacts search` | Agent | Find people by name, nickname or organization.<br>Find a person's handles: `latch contacts search --host mac Ada` |
| `latch contacts lookup` | Agent | The names on cards with each handle (more than one: ambiguous).<br>Who is this number? `latch contacts lookup --host mac +15550000001` |

## Audit

| Command | For | What it does, and an example |
|---|---|---|
| `latch audit` | Host | Read a host's sealed audit log. Owner only. |
| `latch audit verify` | Host | Check every record's place in the chain, without the key. |
| `latch audit rotate-key` | Host | Seal new records under a new key. |
| `latch audit judge` | Host | Show what the gatekeeper asked Plow for one judgment, and the answer. Owner only.<br>See what the gatekeeper was asked and answered: `latch --pretty audit judge REQUEST` |
| `latch audit shot` | Host | Save a browser screenshot an audit record names. Owner only.<br>Save the screenshot after a plan's first action: `latch audit shot ID 1 --output step-1.png` |
