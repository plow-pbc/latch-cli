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
| Login: QR code, the text to send, code countdown | Done | redone in the Mac's layout (centred QR, To/Text table) |
| Login: name taken → replace; expired; error | Done | |
| Privacy | Done | |
| Rules: draft, save | Done | |
| Rules: Gatekeeper example decks (Family, Work) tried with `rules try`, verdicts, rerun | Built | the Mac's Keychain and Messages examples are Mac only |
| Access: Google (`plugins enable google --events`) | eng-65 | the window's step waits on Google working on Windows |
| Access: Contacts & Messages (Full Disk Access, its helper panel) | Mac only | no Messages or Contacts on Windows |
| Start: `serve -d`, run in the background | Done | |
| Start: start when you log in (Login Item); keep awake while plugged in | Built | the user's Run key; keep awake: latch says unsupported on Windows |
| Done: "You're all set" with each step's state | Built | "Text ‹agent›" opens Messages: Mac only |
| Set up again (from Home) | Built | |

## Main window

| Mac | Windows | Notes |
| --- | --- | --- |
| Sidebar: Your account (Agents, Connections), Hosts with state words, "Logged in as" | Built | |
| Sidebar: Plow unreachable notice; viewer-only "Start Hosting on This Mac…" | Built | |
| Host bar: screen tabs, search, inspector toggle | Built | |
| Home: needs you, today, recent activity, on this host, offline host | Done | live: stopped → Start Latch → serving |
| Home: Load Earlier; viewer-only section | Built | |
| Activity: table, filters, agent chip, search, live feed, states | Done | live: this PC's real records |
| Activity: record inspector, gatekeeper request, screenshots | Built | screenshots through run_file |
| Rules: every agent / per agent, edit, save, clear, revisions, try box | Done | live: saved, read back by the CLI, restored |
| Vault: list, search, inspector (no secret values), remove | Done | live: added, shown "Saved, never shown" (the value nowhere on the page), removed; WebView2 autofill off |
| Vault: new item, edit, import (preview, pick, conflicts) | Done / Built | new item live; edit and import built, native Open dialog |
| Host Settings: start/stop/restart, plugins, version, keep awake | Done | live: stopped (asked first) and started; keep awake: latch says unsupported on Windows |
| Agents: list, detail, menu, remove, contact card, hub | Built | Message (opens Messages): Mac only |
| New Agent sheet: catalog, start by text (QR), community, failed | Built | Open in Messages: Mac only |
| Connections: Google accounts and sign-in sheet, devices | Built | Google itself: eng-65 |
| App settings: log out, update check | Built | here a page; on the Mac, a Settings window |
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
| Installer (.dmg/.pkg) and signing | Done / Not started | NSIS, per user (no admin), latch beside the window, Start Menu, uninstall stops the host and the start at login; checked on the VM. Signing waits on a certificate |
| `latch update` | Not started | needs a Windows release feed |
