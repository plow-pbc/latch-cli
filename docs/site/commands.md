# Commands

Every command prints one JSON object. Add `--pretty` to read it yourself.
`latch help COMMAND` lists a command's options, and
`latch help COMMAND --examples` shows how to call it.

Commands marked "Owner only" work from your own machines, never for an
agent.

## Set up

| Command | What it does, and an example |
|---|---|
| `latch setup` | Set this machine up, step by step, at a terminal.<br>`latch setup --status` |
| `latch login` | Log this machine or agent in to Plow: create its key if missing and register it.<br>`latch login --token-file plow.token --name mac` |
| `latch logout` | Log this machine out of Plow.<br>`latch logout --purge` |
| `latch status` | Print this machine, its login and account, and the account's hosts.<br>`latch status --local` |
| `latch devices` | List this account's machines, online or not, or remove one. |
| `latch devices remove` | Remove a machine and sign out the Plow login it registered with. Owner only.<br>`latch devices remove old-pi --yes` |
| `latch update` | Install the newest latch, or the build TAG names. Owner only.<br>`latch update --check` |
| `latch app` | Open the Plow Latch app this latch belongs to (macOS). |
| `latch help` | Help for a command, its examples, or every command as JSON.<br>`latch help browser open` |
| `latch skill` | Print SKILL.md, which teaches an agent how to use latch. |

## Run a host

| Command | What it does, and an example |
|---|---|
| `latch serve` | Run this machine as a host.<br>`latch serve -d` |
| `latch stop` | Stop the host on this state folder; its state is kept. |
| `latch dashboard` | View the running host; Ctrl-C closes only the view. |
| `latch logs` | The host's diagnostics (serve.log).<br>`latch logs --follow` |
| `latch settings set` | Save a setting for this machine's host; needs no login, host or restart.<br>`latch settings set --keep-awake on` |

## Agents

| Command | What it does, and an example |
|---|---|
| `latch agents` | List your agents, with their lines and Plow status. Owner only.<br>`latch agents --host studio` |
| `latch agents show` | Show one agent, by its uid or its exact name. Owner only.<br>`latch agents show Scout` |
| `latch agents catalog` | The kinds of cloud agent Plow can run, for `agents new --cloud`. Owner only. |
| `latch agents lines` | Plow's phone lines, and which are free for a new agent. Owner only. |
| `latch agents new` | Create an agent on a free phone line. Owner only.<br>`latch agents new --own --token-file scout.token --name Scout --line +15550000002` |
| `latch agents remove` | Remove an agent, by its uid or its exact name. Owner only.<br>`latch agents remove Scout --yes` |

## Programs and files

| Command | What it does, and an example |
|---|---|
| `latch exec` | Run one program on a host, inside its sandbox.<br>`latch exec --host pi -- sh -c 'uname -a; ls'` |
| `latch file get` | Copy a file from the host to this machine.<br>`latch file get --host pi build/log.txt ./log.txt` |
| `latch file put` | Copy a file from this machine to the host.<br>`latch file put --host pi ./check.sh scripts/` |

## Browser

| Command | What it does, and an example |
|---|---|
| `latch browser open` | Start a new browser session and load URL.<br>`latch browser --host mac open https://shop.example/` |
| `latch browser observe` | A fresh element list (snapshot), the page's first text, and the tabs.<br>`latch browser --host mac observe` |
| `latch browser text` | The page's text.<br>`latch browser --host mac text` |
| `latch browser screenshot` | A PNG of the page, written to a file on this machine.<br>`latch browser --host mac screenshot page.png` |
| `latch browser do` | Run a plan of up to 10 browser steps, in order.<br>`latch browser --host mac do 'type e3 "Ada Lovelace"' 'click e7 -> path: /cart'` |
| `latch browser act` | Run one browser step on a ref from the last page.<br>`latch browser --host mac act click e7 --check 'text: Added to cart'` |
| `latch browser handoff` | Hand this session to the owner, and wait until they hand it back.<br>`latch browser --host mac handoff --reason captcha` |
| `latch browser find` | Candidate refs by role and exact name; never acts.<br>`latch browser --host mac find --role button --name Download` |
| `latch browser wait` | Wait for an element's state, text or a path, with a deadline.<br>`latch browser --host mac wait --text Saved` |
| `latch browser goto` | Load URL in the session's active tab.<br>`latch browser --host mac goto https://shop.example/cart` |
| `latch browser tab new` | A new tab, on URL or blank; it becomes the active one.<br>`latch browser --host mac tab new https://docs.example/` |
| `latch browser tab switch` | Make tab T (such as t2) the active one.<br>`latch browser --host mac tab switch t1` |
| `latch browser tab close` | Close tab T, or the active one.<br>`latch browser --host mac tab close t2` |
| `latch browser dialog` | Answer the open dialog.<br>`latch browser --host mac dialog accept` |
| `latch browser eval` | Run JavaScript from a file in the page.<br>`latch browser --host mac eval --script-file check.js` |
| `latch browser upload` | Put a file from this machine into a file input, by its ref.<br>`latch browser --host mac upload e4 ./invoice.pdf` |
| `latch browser downloads` | The session's downloads, as handles (d1, d2, …).<br>`latch browser --host mac downloads` |
| `latch browser download get` | Write download D (such as d1) to a file here.<br>`latch browser --host mac download get d1 --output report.csv` |
| `latch browser fill` | Fill a page's fields from a vault item.<br>`latch browser --host mac fill --field username=e4 --field password=e5 --press e6` |
| `latch browser sessions` | Your open sessions, without their ids.<br>`latch browser --host mac sessions` |
| `latch browser close` | End the session, deleting its cookies and logins.<br>`latch browser --host mac close` |
| `latch browser setup` | Download and verify a browser engine into this machine's state folder. Owner only.<br>`latch browser setup --engine camoufox` |

## Vault

| Command | What it does, and an example |
|---|---|
| `latch vault list` | Every item's metadata, a login's username included (never a secret value).<br>`latch vault --host mac list` |
| `latch vault show` | One item's metadata and its non-secret values (never a secret one). Owner only.<br>`latch vault --host mac show v3` |
| `latch vault add` | Add an item to the vault. Owner only.<br>`latch vault add --kind login --title Shop --site shop.example --field username=ada --stdin` |
| `latch vault edit` | Change an item's title, sites or fields, at its current revision. Owner only.<br>`latch vault edit v1 --revision 2 --stdin` |
| `latch vault link` | Add an item from your phone, through a one-time link. Owner only.<br>`latch vault --host mac link --kind card --title Visa --page https://phone.example/latch` |
| `latch vault remove` | Remove an item at its current revision. Owner only.<br>`latch vault --host mac remove v3 --revision 1` |
| `latch vault import` | Import another password manager's export. Owner only.<br>`latch vault --host mac import export.1pux` |

## Rules

| Command | What it does, and an example |
|---|---|
| `latch rules set` | Replace the rules with TEXT, or with the text of --file F (- reads stdin). Owner only.<br>`latch rules set 'Agents may search my mail but never send any.'` |
| `latch rules edit` | Change the rules in $EDITOR; saved only if they did not change meanwhile. Owner only.<br>`latch rules edit --agent ada` |
| `latch rules clear` | Remove the rules. Owner only.<br>`latch rules clear --agent ada --revision '9f2c…'` |
| `latch rules show` | The rules for every agent and each agent's own, with their revisions. Owner only. |
| `latch rules try` | Preview the gatekeeper's verdict on a request, without running it. Owner only.<br>`latch rules try --agent Penny -- exec --write ~/site -- npm run build` |

## Connectors

| Command | What it does, and an example |
|---|---|
| `latch plugins enable` | Turn a connector on, if its setup succeeds. Owner only.<br>`latch plugins enable google` |
| `latch plugins disable` | Turn a connector off. Owner only.<br>`latch plugins disable google` |
| `latch google -- COMMAND` | Run one allowed command on a host's Google account. Calendar: `calendars`, `conflicts`, `create`, `delete`, `event`, `events`, `freebusy`, `move`, `respond`, `search`, `update`. Gmail: `archive`, `attachment`, `drafts create`, `drafts delete`, `drafts forward`, `drafts get`, `drafts list`, `drafts reply`, `drafts reply-all`, `drafts send`, `drafts update`, `forward`, `get`, `labels list`, `mark-read`, `messages modify`, `messages search`, `reply`, `reply-all`, `search`, `send`, `thread get`, `unread`.<br>`latch google --host mac --account ada@example.com -- gmail search 'from:grace newer_than:7d'` |
| `latch messages search` | Search messages for PHRASE, newest first.<br>`latch messages search --host mac --since 10m code` |
| `latch messages thread` | One conversation, oldest first, ending with its newest message.<br>`latch messages thread --host mac --with +15550000001` |
| `latch messages chats` | Conversations, most recently active first, with who is in them.<br>`latch messages chats --host mac --kind group` |
| `latch messages unreplied` | Direct conversations whose newest message is inbound: candidates, not proof a reply is owed.<br>`latch messages unreplied --host mac --within 24h` |
| `latch contacts search` | Find people by name, nickname or organization.<br>`latch contacts search --host mac Ada` |
| `latch contacts lookup` | The names on cards with each handle (more than one: ambiguous).<br>`latch contacts lookup --host mac +15550000001` |

## Audit

| Command | What it does, and an example |
|---|---|
| `latch audit` | Read a host's sealed audit log. Owner only.<br>`latch audit --before ID` |
| `latch audit verify` | Check every record's place in the chain, without the key. |
| `latch audit rotate-key` | Seal new records under a new key. |
| `latch audit judge` | Show what the gatekeeper asked Plow for one judgment, and the answer. Owner only.<br>`latch --pretty audit judge REQUEST` |
| `latch audit shot` | Save a browser screenshot an audit record names. Owner only.<br>`latch audit shot ID 1 --output step-1.png` |
