# Contributing to Latch

Thank you for helping. This page says how to build Latch, run its tests and
propose a change.

## What you need

- Rust. `latch/rust-toolchain.toml` pins the version, and `rustup` installs
  it for you.
- [just](https://github.com/casey/just), [cargo-nextest](https://nexte.st)
  and Python 3.
- For the Linux tests on a Mac: Docker.
- For the Mac app's tests: macOS with Xcode 26 or later.

## Build

```sh
just build
```

This builds the checkout into `.dev/bin/latch`. That build keeps its state in
`.dev/state`, so it never touches `~/.latch` or an installed Latch. It talks
to a local Plow stack, as [AGENTS.md](AGENTS.md) describes.

To try your build against Plow itself, build with `cargo build` in `latch/`.
Give it a state folder of its own, and tell it which Plow to use:

```sh
latch/target/debug/latch --state-dir .dev/state-mine login --api-url https://api.plow.co
```

## Test

```sh
just test             # everything a merge needs, on this OS
just test --linux sandbox  # one suite on Linux, in a Docker container
just test core        # one suite: the quick loop
just test-list        # every suite, what it covers and what it needs
```

The gate is `cargo fmt`, `cargo clippy` with warnings as errors, and the
tests, on macOS and on Linux. CI runs the same commands, and runs the whole
Linux gate on every pull request that changes code. Run `just test --linux` yourself only for a
Linux-specific change (the bubblewrap sandbox, the Linux browser code, the
installer's Linux paths, systemd) or to reproduce a Linux failure from CI,
and then only that suite or test (`just test --linux --only NAME`; the
systemd tests run in CI and on a Linux machine, not in the container). A suite
that can't run where you asked fails and says why: nothing is skipped.
The Linux container gets 6 CPUs and 8 GB; set `LATCH_LINUX_CPUS` or `LATCH_LINUX_MEMORY` to change that.

Some checks call a real Plow stack or the decision model (`just dev-login`,
`just gatekeeper`). They aren't part of the gate. Maintainers run them by
hand.

## Propose a change

1. For anything large, open an issue first and describe the problem.
2. Make the change on a branch off `main`, in one pull request. Keep it to
   one change.
3. Update the docs in the same pull request when behavior changes:
   [README.md](README.md), [docs/](docs/) and the guides in `latch/*.md`.
4. Run `just test`; CI runs the Linux gate. Say in the pull request which
   ones you ran.
5. Write the pull request's title and description so that someone who hasn't
   seen the discussion can follow them.

[AGENTS.md](AGENTS.md) sets the engineering standard: the simplest change
that solves the real problem, no compatibility layers, and tests that check
behavior. [REVIEW.md](REVIEW.md) says what reviewers look for.

Report security problems privately, as [SECURITY.md](SECURITY.md) says, not
in a public issue.

## License

Latch is licensed under Apache-2.0 ([LICENSE](LICENSE)). Unless you say
otherwise, what you contribute is licensed under the same terms, as section 5
of the license says.
