# Security

## Report a vulnerability

Please report security problems privately, not in a public issue. On
GitHub, open this repository's **Security** tab and choose **Report a
vulnerability**. Include:

- the output of `latch --version` on each machine involved;
- what you did, what happened, and what you expected;
- whether an agent, one of your own machines or the host itself made the
  call.

Don't include real passwords, tokens or message contents.

## Security model

Latch lets an agent use a machine its owner trusts: its files, programs,
browser and connectors. That machine is the **host**. Three ideas hold the
model up.

### 1. The host judges every call that acts, for a caller it has authenticated

- **The caller is its key.** Every machine and agent has its own key. The
  host asks Plow who a key is when the caller connects, and again before
  every operation. A name is only a label. If Plow says no, or can't be
  reached, the call is refused.
- **Work that acts is judged first.** Programs, file copies, Google,
  Messages, Contacts and the browser's risky steps go to the host's
  gatekeeper before they run. The gatekeeper is a model. It reads the
  request, the owner's rules and, for an agent, the owner's messages in
  their Plow chat with that agent.
- **It fails closed.** If the gatekeeper can't judge, nothing runs.
- **Settings are the owner's alone.** Only the owner's own machines can read
  or change a host's rules, change its vault, or turn its connectors on and
  off. Agents are refused.
- **An agent can list the vault, but never read its secrets.** `vault list`
  shows an agent each item's id, title and saved sites, and the gatekeeper
  judges that request like the agent's other work. No username or secret
  value reaches the agent.
- **The gatekeeper is not a guarantee.** It is a model, and models make
  mistakes. Rules are judged, not enforced. `latch serve --deny-exec` is
  enforced in code: no agent runs a program on that host.

### 2. The work runs confined

- Programs, file copies and connector calls run in a sandbox: Seatbelt on
  macOS, bubblewrap and seccomp on Linux. A host that can't set up its
  sandbox refuses to start.
- A program reads home and the system's own files, but never the host's
  state folder. It writes only to the folders the call names, its own
  private temp folder, and tool caches that already exist, such as
  `~/.cache` and `~/.npm`. It has no network unless the call asks for it.
- Vault values go into a web page without reaching the agent. A saved login
  fills only on its own sites.

### 3. The data an agent handles is untrusted

Message bodies, contact names, web pages and file contents can carry a
stranger's words. Latch treats them as data, never as instructions or
permission:

- Only the owner's rules, the built-in policy and the chat messages Plow
  marks as the owner's count as permission.
- Nothing an agent reads can change a rule, a sandbox policy or how a
  browser starts.

## Known limits

These are part of the design. Reports that a change makes one of them worse
are welcome.

- **The host trusts its own OS user.** Anything running as the host's OS
  user can use the host's local socket and act as the owner, including
  changing its rules and vault. Run a host only where you trust everything
  that runs as that user.
- **An owner's login placed where an agent runs is judged as the owner.**
  The host can't tell. Give each agent a login of its own.
- **A script the agent ran on a page can catch a value filled there later.**
  An agent's `eval` runs with the page's own access, and what it leaves
  behind (an event listener, a service worker, a stored script) can read a
  vault value that Latch fills on that site later in the session. Latch
  doesn't track this. Instead the gatekeeper judges each script's whole
  source before it runs: one that reads form fields, listens to input,
  keyboard or form events, or sends page data off the page needs the
  owner's explicit OK. That judgment is a model's, so a script that hides
  what it does may get through. A new browser session no longer starts
  clean: each agent's sessions on a host share one persistent profile, so
  what a script stored for a site (a service worker, site storage) is still
  there in that agent's later sessions. Only its tabs start fresh. Answers
  mask the vault's current card numbers and identity secrets in every
  session; a value a site saved before you changed or removed it in the
  vault isn't masked.
- **On macOS, the browser engine runs outside Latch's sandbox**, in its own
  sandbox. A compromised engine could read the host's state folder.
- **Outside the signed macOS app, secrets are files** that only the OS user
  can read (mode 600). The signed app keeps them in the Keychain.
- **The audit log keeps program arguments in full.** Each record is
  encrypted. Where commands are shown to people, credential-looking values
  are hidden, best effort. Keep credentials out of arguments: use
  `--secret-env`.
- **The gatekeeper does not remember earlier calls.** The same request sent
  twice is judged twice.

[How Latch works](docs/HOW-LATCH-WORKS.md) describes the trust model in more
detail.
