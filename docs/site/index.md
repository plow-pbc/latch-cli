# Install and first run

Latch lets your AI agents use a computer you trust: its files, programs and
browser, your Google account, and on a Mac its Messages and Contacts. That
computer runs a Latch host. Its gatekeeper checks every request from your
agents against your rules before it runs.

Latch runs on macOS 13 or later and on 64-bit Linux. You need a Plow account.

## Install

```sh
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh
```

On a Mac this installs the Plow Latch app and the `latch` command. On Linux
it installs the `latch` command, and may ask for your password once to set up
the sandbox.

If `latch` isn't found afterwards, open a new terminal. The installer puts
`~/.local/bin` on your `PATH`, or says what to add for your shell.

## Two kinds of install

**This computer hosts.** It protects what's on it and does your agents' work.
Install it on the place you trust: your Mac, a Linux server or a Raspberry
Pi. That's the install above.

**Client only: you just want to reach your other hosts.** This computer hosts
nothing, so it needs only the `latch` command:

```sh
sh install.sh --client-only
```

## First run

On a Mac with macOS 26 or later, the installer opens the Plow Latch app,
which walks you through these steps. Everywhere else, setup runs in the
terminal. To run it again yourself:

```sh
latch setup
```

1. **Log in to Plow.** Text the code it shows from your Plow phone, or scan
   the QR code.
2. **Connect Google.** Optional. On a Mac, Google needs macOS 15 or later.
3. **Turn on Messages and Contacts.** Mac only. Grant Full Disk Access to
   Plow Latch, not to Terminal.
4. **Start the host.** This computer now answers your agents.

Each step is skipped once it's done, so you can run setup again at any time.

Then write your rules: what your agents may and may not do, in your own
words. See [How Latch works](how-latch-works.md#the-gatekeeper-and-rules).

```sh host
latch rules edit
```

The Mac app is built entirely on the `latch` command: anything you do in the
app, you can do with `latch`.

## Check it worked

```sh
latch
```

It shows this computer at a glance: your account, whether it is hosting, your
hosts, its connectors and one next step.

## Update

```sh
latch update
```

This installs the newest release. A host running in the background restarts
on it. A running host tells you when an update is out, and nothing installs
on its own.
