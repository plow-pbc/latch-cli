# Install and first run

Latch lets your AI agents use a machine you trust: its files, programs and
browser, your Google account, and on a Mac its Messages and Contacts. That
machine runs a Latch host. Its gatekeeper checks every request from your
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

| Your Mac runs | What happens next |
|---|---|
| macOS 26 or later | The Plow Latch app opens and takes you through setup. |
| macOS 15 to 25 | Setup runs in the terminal. |
| macOS 13 or 14 | Setup runs in the terminal. Google needs macOS 15, so it is left out. |

## Two kinds of install

**A host** protects what's on its machine and does your agents' work. Install
it on the place you trust: your Mac, a Linux server or a Raspberry Pi.

```sh
sh install.sh
```

**An agent's machine** needs only the `latch` command. It calls your hosts,
and protects nothing itself.

```sh
sh install.sh --client-only
```

Then [connect the agent](connect-an-agent.md).

## First run

```sh
latch setup
```

Setup takes these steps. Each one is skipped once it's done, so you can run
setup again at any time.

1. **Log in to Plow.** Text the code it shows from your Plow phone, or scan
   the QR code.
2. **Connect Google.** Optional.
3. **Turn on Messages and Contacts.** Mac only. Grant Full Disk Access to
   Plow Latch, not to Terminal.
4. **Start the host.** This machine now answers your agents.

Then write your rules: what your agents may and may not do, in your own
words.

```sh
latch rules edit
```

## Check it worked

```sh
latch
```

It shows this machine at a glance: your account, whether it is hosting, your
hosts, its connectors and one next step.

## Update

```sh
latch update
```

This installs the newest release. A host running in the background restarts
on it. A running host tells you when an update is out, and nothing installs
on its own.
