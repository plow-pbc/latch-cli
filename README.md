# Latch

Latch lets your AI agents use a machine you trust: its files, programs and
browser, your Google account through it, and on a Mac its Messages and
Contacts. That machine runs a Latch **host**. Your agents call it from
wherever they run. The host's **gatekeeper** checks each call against your
rules and what you told the agent in Plow. The host keeps a sealed audit log
of every call.

Latch runs on macOS 13 or later and on 64-bit Linux (x86_64 or arm64). You
need a [Plow](https://plow.co) account.

## Install

```sh
curl -fsSLO https://releases.plow.co/latch-cli/install.sh && sh install.sh
```

- On a Mac, this installs the Plow Latch app and the `latch` command.
- On Linux, it installs the `latch` command.
- `latch` goes in `~/.local/bin`. If that folder isn't on your `PATH`, the
  installer adds it in your shell's login profile and prints the line for
  the terminal you are in.

Check it worked:

```sh
latch --version
```

## First run

The first install starts setup for you. On macOS 26 or later it opens the
Plow Latch app instead, which takes you through the same steps. To run setup
again at any time:

```sh
latch setup
```

Setup logs this machine in to Plow: you text a code from your phone. It then
connects Google, turns on Messages and Contacts on a Mac, and offers to start
the host.

Then run `latch` alone to see this machine at a glance.

## Connect an agent

- **An agent that runs on Plow** starts when you text Plow's number from your
  phone: `latch agents start-by-text` says what to send, and the Mac app's
  New Agent shows it as a QR code. It has its own credential in its
  environment. If its machine has no Latch yet, install it there with
  `sh install.sh --client-only`, then run `latch login`.
- **An agent you run yourself** gets its own credential from Plow:

  ```sh
  latch agents new --own --name NAME --line LINE --token-file FILE
  ```

  Give `FILE` to the agent. Its program logs in with
  `PLOW_TOKEN_FILE=FILE latch login`.
- **Never copy your own login to an agent.** A host treats your login as
  you, so it would not judge the agent's calls as the agent's.

Agents learn how to use Latch from `latch skill`.

## Read more

- [Getting started](docs/GETTING-STARTED.md): install, log in, start a host,
  call it, connect an agent.
- [How Latch works](docs/HOW-LATCH-WORKS.md): hosts, owners and agents, the
  gatekeeper, trust and limits.
- Guides: [browser](latch/BROWSER.md), [vault](latch/VAULT.md),
  [Google](latch/GOOGLE.md), [macOS](latch/MACOS.md),
  [Docker](latch/DOCKER.md), [the JSON an app reads](latch/APP.md).
- In the terminal: `latch --help`, and `latch help COMMAND --examples`.
- [Security](SECURITY.md) and [contributing](CONTRIBUTING.md).

## License

Apache-2.0: see [LICENSE](LICENSE) and [latch/NOTICE](latch/NOTICE).
Copyright 2026 The Plow Collective, Inc. Code adapted from other projects
keeps its own license, named in NOTICE beside its text (`latch/LICENSE-*`).
Release archives and Plow Latch.app carry LICENSE, NOTICE and those texts.

"Plow" and the Plow logo are trademarks of The Plow Collective, Inc. The
license grants no trademark rights.
