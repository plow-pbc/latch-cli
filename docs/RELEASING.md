# Releasing Latch

A release is cut from `main` by maintainers. Every release gets release notes
written for the people who use Latch.

## Cut a release

1. Run the release workflow on `main`:
   `gh workflow run release.yml -R plow-pbc/latch-cli --ref main`. It names
   the release `v<version>-<run number>`, builds, signs and checks every
   archive, runs the macOS tests, uploads the release to releases.plow.co and
   leaves a draft release on GitHub.
2. When the run is green, write the notes (below) and publish the draft:
   `gh release edit <tag> --notes-file NOTES.md --draft=false --latest`.
   Publishing approves the release: it runs the promote workflow, which makes
   it the release `install.sh` and `latch update` install.
3. Check that the promote run is green and that
   `https://releases.plow.co/latch-cli/latest.json` names the new tag.

## Release notes

Write the notes from the pull requests merged since the previous release
(`git log --first-parent --merges <previous tag>..<tag>`), reading each one's
title and description.

- **Group by what changed for people**, for example Cards, Phone links,
  Hosts, Gatekeeper, Browser, Mac app, Install and update, Fixes. Use only the
  groups a release needs.
- **One or two short lines per pull request**, saying what changes for
  owners and agents, and why it matters to them. Several pull requests that
  make one change can share a line.
- **Link every pull request**, as `([#123](https://github.com/plow-pbc/latch-cli/pull/123))`.
  Every pull request in the range appears once.
- **Behind the scenes**: test-only and CI-only pull requests share one short
  line at the end, such as "Steadier tests", with their links.
- **No internal jargon**: no spec sections or chunks, review rounds,
  function or file names, or test names. Name a command or flag only when
  people type it.
- **Keep it short**, and keep the workflow's last line,
  ``Built from `main` @ `<commit>`.``

### Example: v0.1.0-154

```markdown
## Cards
- A card is never filled on a site where the agent's own script ran earlier in the session. The agent starts a fresh session instead. ([#326](https://github.com/plow-pbc/latch-cli/pull/326))
- A filled card's number reaches the agent only as its last four digits (•••• 4242), in every answer and in the saved history. ([#327](https://github.com/plow-pbc/latch-cli/pull/327), [#347](https://github.com/plow-pbc/latch-cli/pull/347))
- After a card is filled, the agent's scripts are refused only until the page is submitted, the same guard passwords have. ([#347](https://github.com/plow-pbc/latch-cli/pull/347))
- The gatekeeper judges a card payment on the checkout itself: the page, where the number goes, the button pressed and the amount shown. ([#328](https://github.com/plow-pbc/latch-cli/pull/328))

## Setup
- `latch setup` offers starter rules for your agents: a family assistant's, a work assistant's, or none. It asks only when the computer has no rules yet. ([#340](https://github.com/plow-pbc/latch-cli/pull/340))

## Browser
- Agents now know a host can have two browsers, Chrome and Camoufox. When a site silently drops a sign-in, they try it once on the other. ([#346](https://github.com/plow-pbc/latch-cli/pull/346))

## Behind the scenes
- Tests for paying with a saved card on real checkout layouts, including payment providers' card fields. ([#325](https://github.com/plow-pbc/latch-cli/pull/325))

Built from `main` @ `043ceb1f541e4e112288d59390dcb18181f9be3d`.
```
