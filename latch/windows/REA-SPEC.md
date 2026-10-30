# Plow Latch for Windows: what the Mac app does (REA pass)

The Windows app is rebuilt from the shipped Mac app, read with REA (Reverse Engineer
Anything, `rea-agents` 6.8.0 = morluto/rea `250dc81`) on Ghidra 12.1.4. The target is
release `v0.1.0-154` (`latch-v0.1.0-154-aarch64-apple-darwin.tar.gz`, SHA-256 checked
against the release's `SHA256SUMS`). The source was used only afterwards, to check
REA's findings; where the two differ, it says so below.

## What REA found in the bundle

`Plow Latch.app` has two binaries:

- `Contents/MacOS/latch`: the Rust CLI, Mach-O arm64. It is the same program Linux
  ships bare (`inspect-rust-binary`).
- `Contents/Frameworks/libPlowLatchUI.dylib`: the window, a universal Mach-O
  (x86_64 + arm64). Ghidra 12.1 cannot open a universal file, so the arm64 slice was
  cut out and analyzed alone.

The dylib holds 5,773 Swift symbols: 4,261 `PlowLatchUI`, 652 `PhoneNumberKit`, and
the rest SwiftUI and Foundation (`inspect-native-dispatch-metadata`). Its strings
(`search --kind strings`) give each screen's copy, every error message, the
onboarding examples, SF Symbol names and the field names of the JSON it decodes.

**What REA missed.** Swift stores strings of 15 bytes or fewer inline in instructions,
so the short arguments the window passes to `latch` (`["rules", "show"]`,
`["status", "--local"]`, ...) are not in its string table. Only longer ones are, such
as `--other-top-site` and `--no-other-top-sites`. The source has 36 calls through
`LatchClient.run` and `stream`, listed below. REA's other findings all match the
source.

## How the window works

The window holds no logic of its own. Every screen runs the `latch` beside it as a
subprocess and decodes its JSON (`LatchProcess`, `LatchClient`). Long views follow a
stream instead: `status --local --follow`, `audit --follow`, `login --events` and
`plugins enable google --events`. Error codes from latch (`host_unreachable`,
`plow_unreachable`, `gatekeeper_denied`, `token_file_exists`, ...) map to fixed copy
(`ErrorCopy.swift`; REA recovered all of it).

The Windows app keeps this model. A Tauri 2 shell runs the native Windows
`latch.exe` that ships beside it, with the same arguments, and renders the same
screens in a webview.

## Screens

**Onboarding.** The steps are `welcome, login, privacy, rules, access, start, done`
(REA: string table; source: `App/Progress.swift`). Each step is marked
`continued` or `skipped`.

| Step | Mac behavior | Windows |
| --- | --- | --- |
| welcome | "Welcome to Latch", the brand mark (an SVG path, in the strings) | same |
| login | `login --events [--name N] [--replace]`. It shows a QR code and the text to send ("Scan with your phone's camera, or text the code below"), counts down the code's expiry, and handles "This Mac's name is taken" with a replace option | same; "Mac" becomes "PC" |
| privacy | "You decide what your agents do": checked before it runs, a sealed log, encrypted end to end | same |
| rules | Meet the Gatekeeper. Example decks (Family assistant, Work assistant) are tried against draft rules with `rules try`, then saved with `rules set --file -` | same |
| access | Google (`plugins enable google --events`), Contacts & Messages (Full Disk Access, macOS only) | Google only. Messages and Contacts don't exist on Windows |
| start | `serve -d`, "Start when you log in" (a launchd LaunchAgent through ServiceManagement), "Run in background" | `serve -d` as a per-user Windows service substitute (a Task Scheduler logon task); start at login through the same task |
| done | "Your agents can now use this Mac." | same, "PC" |

**Main window.** A sidebar lists account pages (Agents, Connections) and, for each
host, `home, activity, rules, vault, settings` (REA: strings; source:
`Main/Hosts.swift`).

- **Home:** the host's state from `status --local` (running, ready, managed, blocked),
  recent requests, "Nothing needs you right now", the update notice.
- **Activity:** `audit --follow` records with All / Allowed / Refused filters, an
  agent filter, and a record inspector (caller, decision, layers, command, stdout and
  stderr byte counts, screenshot).
- **Rules:** `rules show`/`set`/`clear`, rules for every agent and per agent, and a
  try box (`rules try`, "about $0.0001" per try).
- **Vault:** `vault list --all`/`show`/`remove`/`import --preview|--pick`, with the
  login, card, identity and note kinds. Secret values are never shown.
- **Settings (host):** start, stop and restart, keep-awake ("while plugged in"),
  plugins (`plugins enable|disable`).
- **Agents:** `agents`, `agents catalog`, `agents start-by-text` (QR code plus
  "Open in Messages"), `agents remove`, `agents card`.
- **Connections:** Google accounts (`plugins enable google --events --add-account`) and
  logged-in devices (`devices`, `devices remove`).
- **App settings:** `logout --yes`, `update --check`.

## The commands the window runs (source, `Latch/LatchClient.swift`)

`status`, `status --local [--follow]`, `status --host H`, `setup --status`,
`login --events`, `logout --yes`, `stop`, `update --check`, `rules show|set|clear`,
`plugins enable|disable`, `vault list|show|remove|import`, `agents`,
`agents card|catalog|remove|start-by-text`, `devices`, `devices remove`, `audit`,
`audit --follow`, `audit shot`.

## Mac-only pieces and their Windows replacements

| Mac | Windows |
| --- | --- |
| SwiftUI window loaded into `latch` from the bundle | Tauri 2 app (`Plow Latch.exe`) beside `latch.exe` |
| Keychain (data-protection, keychain group) | DPAPI-protected secret files |
| launchd agent / ServiceManagement login item | Task Scheduler logon task running `latch serve --foreground` |
| Seatbelt worker sandbox | AppContainer + Job Object (later milestone; until then workers refuse, as on any unsupported OS) |
| Full Disk Access, Messages, Contacts | none |
| `IOPMAssertion` keep-awake | `SetThreadExecutionState` |
| Quick Look, "Open in Messages" | open with the default app; QR code only |
| codesign + notarization, `.app` in a tarball | Authenticode, NSIS/MSI from the Tauri bundler |
