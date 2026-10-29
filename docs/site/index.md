# Install and first run

Latch lets your AI agents use a computer you trust: its files, programs,
browser, Google account and, on a Mac, Messages and Contacts. That computer is
a host. Its gatekeeper checks each request against your rules first.

You need macOS 13 or later, or 64-bit Linux, and a Plow account.

## Install

Download the installer:

```sh host agent
curl -fsSLO https://releases.plow.co/latch-cli/install.sh
```

Then install one of two ways:

<!-- two-up -->
**This computer hosts.**

```sh host
sh install.sh
```
<!-- next -->
**The computer your agent runs on.** Then [connect the agent](connect-an-agent.md).

```sh agent
sh install.sh --client-only
```
<!-- /two-up -->

You can also install the client-only way on your own laptop, just to reach
your hosts.

`latch` not found? Open a new terminal.

## First run

The installer starts setup. On macOS 26 or later, the Plow Latch app opens
and walks you through it.

1. **Log in to Plow.** Text the code it shows from your Plow phone.
2. **Connect Google.** Optional.
3. **Turn on Messages and Contacts.** Mac only. Grant Full Disk Access to
   Plow Latch.
4. **Start the host.**

Then write your rules: what your agents may and may not do.

```sh host
latch rules edit
```

The Mac app runs on the `latch` command: anything it does, `latch` can do.

## Check it worked

```sh host
latch
```

## Update

```sh host
latch update
```

Nothing updates on its own.
