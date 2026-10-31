# Security

Latch lets an agent work on your computer without handing it the keys.

## How it protects you

- **Every caller is a key.** A host asks Plow who each key is, before every
  operation. A name is only a label.
- **Work is judged first.** Programs, file copies, Google, Messages, Contacts
  and risky browser steps go to the gatekeeper before they run. If it can't
  judge a step, that step doesn't run.
- **Work runs in a sandbox:** Seatbelt on macOS, bubblewrap on Linux. A
  program can't read the host's state folder. It writes only to the folders
  the request names, its own temp folder and existing tool caches, and it has
  no network unless the request asks for it.
- **Secrets stay on the host.** Vault values go into web pages without
  reaching the agent. An agent can list the vault, but never read its secrets.
- **What an agent reads is data.** Messages and web pages can't give an agent
  permission, change a rule or change the sandbox.
- **Every request is recorded** in a sealed audit log, which
  `latch audit verify` checks.

## Known limits

- **Your host trusts its own user.** Anything running as that user on the
  host can act as you there. Run a host only where you trust everything that
  runs as that user.
- **The gatekeeper is a model,** and models make mistakes. Rules are judged,
  not guaranteed. `latch serve --deny-exec` is enforced in code.
- **A page script can catch a filled value.** If an agent ran its own script
  on a site, what it left behind can read a vault value filled there later in
  the same browser session. The gatekeeper judges each script's whole source
  before it runs, and one that reads form fields, listens to typing or sends
  page data away needs your explicit OK. That judgment is a model's, so a
  script that hides what it does may get through. A new session starts clean.
- **Latch protects a secret until the form is submitted or the fields are
  cleared.** What a site shows afterwards, such as a confirmation that
  repeats your card number, is the site's, and your agent reads it as you
  would.
- **Your own login is you.** A host judges it as you, even where an agent
  runs. Give each agent its own login.

## Report a vulnerability

Report it privately, not in a public issue. See
[SECURITY.md](https://github.com/plow-pbc/latch-cli/blob/main/SECURITY.md)
on GitHub.
