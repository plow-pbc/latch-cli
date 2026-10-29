# Install and first run

Latch lets your AI agents use a computer you trust: its files, programs,
browser, Google account and, on a Mac, Messages and Contacts. That computer is
a host. Its gatekeeper checks each request against your rules first.

You need macOS 13 or later, or 64-bit Linux, and a Plow account.

## Install on a host

```sh host
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh
```

## Install on an agent

On the computer your agent runs on. Then [connect the agent](connect-an-agent.md).

```sh agent
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh --client-only
```

On a Mac, Latch installs `/Applications/Plow Latch.app` and links `latch` to it
at `~/.local/bin/latch`; on Linux, `latch` goes in `~/.local/bin`.
The installer adds `~/.local/bin` to your `PATH`: open a new terminal to pick it up.

## First run

The installer starts setup. On macOS 26 or later, the Plow Latch app opens
and walks you through it.

1. **Log in to Plow.** Text the code it shows from your phone.
2. **Optional: connect Google.**
3. **Optional, Mac only: turn on Messages and Contacts.** Grant Full Disk
   Access to Plow Latch.
4. **Start the host.**

Then write your rules: what your agents may and may not do.

```sh host
latch rules edit
```

The Mac app runs on the `latch` command: anything it does, `latch` can do.

## Check it worked

On a host or an agent's computer, see its login and the hosts it can reach:

```sh host agent
latch status
```

## Update

On a host:

```sh host
latch update
```

An agent can't run `latch update`, so on an agent's computer, install again:

```sh agent
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh --client-only
```

Nothing updates on its own.
