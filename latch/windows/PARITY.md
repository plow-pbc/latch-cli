# Plow Latch on Windows: parity with the Mac app

Every screen, flow and feature of the Mac app (`latch/gui`, read with REA: REA-SPEC.md), and where
the Windows one stands. Status key:

- **Done**: built and checked on the Windows VM.
- **Built**: written and checked against the Mac's stills, not yet on the VM.
- **In progress**: being ported now.
- **Not started**: not begun.
- **eng-65**: owned by the second engineer (Google, browser, DPAPI).
- **Mac only**: has no Windows meaning.

The Mac's own Snapshots tool (`swift run Snapshots`) renders every state below on made-up data; the
Windows window's fake mode shows the same data, for side-by-side stills.

## Setup (onboarding)

| Mac | Windows | Notes |
| --- | --- | --- |
| Welcome, with the brand mark | Done | |
| Login: QR code, the text to send, code countdown | Done | |
| Login: name taken → replace; expired; error | Done | |
| Privacy | Done | |
| Rules: draft, save | Done | |
| Rules: Gatekeeper example decks (Family, Work) tried with `rules try`, verdicts, rerun | Not started | |
| Access: Google (`plugins enable google --events`) | eng-65 | the window's step waits on Google working on Windows |
| Access: Contacts & Messages (Full Disk Access, its helper panel) | Mac only | no Messages or Contacts on Windows |
| Start: `serve -d`, run in the background | Done | |
| Start: start when you log in (Login Item) | Not started | Windows: a Task Scheduler logon task |
| Done (text an agent, skipped steps, not running) | Done | the plain "done"; its other states not started |
| Set up again (from Home) | Built | |

## Main window

| Mac | Windows | Notes |
| --- | --- | --- |
| Sidebar: Your account (Agents, Connections), Hosts with state words, "Logged in as" | Built | |
| Sidebar: Plow unreachable notice; viewer-only "Start Hosting on This Mac…" | In progress | notice built; viewer-only not started |
| Host bar: screen tabs, search, inspector toggle | Built | |
| Home: needs you, today, recent activity, on this host, offline host | Built | |
| Home: viewer-only section, Load Earlier | Not started | |
| Activity: table, filters, agent chip, search, live feed, load earlier, states | In progress | |
| Activity: record inspector, gatekeeper request, screenshots | In progress | |
| Rules: every agent / per agent, edit, save, clear, revisions, try box | In progress | |
| Vault: list, search, inspector (no secret values), remove | In progress | |
| Vault: new item, edit, import (preview, pick, conflicts) | In progress | native Open dialog added for import |
| Host Settings: start/stop/restart, plugins, version, keep awake | In progress | |
| Agents: list, detail, menu, remove, contact card, hub | In progress | |
| New Agent sheet: catalog, start by text (QR), community, failed | In progress | |
| Connections: Google accounts and sign-in sheet, devices | In progress | Google itself: eng-65 |
| App settings: log out, update check | In progress | here a page; on the Mac, a Settings window |
| Menu commands: New Item, Import, Save, Find, Show Sidebar/Inspector, screens ⌘1–5 | Not started | Windows: the same as Ctrl shortcuts |
| Quick Look for screenshots and cards | Not started | Windows: open with the default app |

## What runs underneath

| Mac | Windows | Notes |
| --- | --- | --- |
| latch host, local socket, owner-only state | Done | #355 |
| Worker sandbox (Seatbelt) for exec and file get/put | Done | #359: AppContainer + Job object |
| Google (gog) | eng-65 | |
| Browser sessions | eng-65 | |
| Vault secrets in the Keychain | eng-65 | DPAPI |
| Messages and Contacts readers | Mac only | |
| Keep awake | Not started | Windows: SetThreadExecutionState |
| Installer (.dmg/.pkg) and signing | Not started | NSIS + Authenticode |
| `latch update` | Not started | needs a Windows release feed |
