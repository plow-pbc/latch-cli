# Plow Latch for macOS

A native face on the latch CLI: it runs the `latch` bundled inside it and shows
what it prints. It never reads latch's state files, holds a token, or calls Plow.
Requires macOS 26.

## Layout

- `Sources/PlowLatch/`: the app (scenes, app delegate).
- `Sources/PlowLatchKit/`:
  - `Latch/`: `LatchClient`, the only code that knows a CLI exists. It has two implementations:
    - `LiveLatchClient` runs the bundled `latch`;
    - `FakeLatchClient` answers from sample JSON.
  - `Model/`: `AppModel` and the typed CLI output.
  - `Features/`: the screens, the menu bar extra and Settings.
  - `Theme/`: every colour, font, spacing, motion and surface choice.
  - `App/WindowRouter.swift`: the one way windows open.
- `Sources/Snapshots/`: renders every screen and state with the fake client.

## Build, test, run

```sh
swift test                          # model, decoding, and the live client against scripts/stub-latch
scripts/build-app.sh                # build/Plow Latch.app, with the repo's latch (release) inside
scripts/build-app.sh --latch PATH   # ... with another latch
"build/Plow Latch.app/Contents/MacOS/PlowLatch" -LatchStateDir "$PWD/../../.dev/mac-app/dev"
"build/Plow Latch.app/Contents/MacOS/PlowLatch" -LatchFake serving   # sample data, no CLI
swift run Snapshots ../../.dev/mac-app/snapshots
scripts/measure.sh                  # launch, idle, menu-bar and window checks against the budget
```

Launch arguments set these defaults for one run:
- `-LatchStateDir`: the state folder passed as `--state-dir`.
- `-LatchExecutable`: run another latch.
- `-LatchFake <serving|stopped|loggedOut|noActivity|hostError|unreachable>`: use the sample data instead of a CLI.
- `-KeepInMenuBar YES`: the app leaves the Dock when its last window closes.

Dev runs use a state folder under the repo's `.dev/`, never `~/.latch`.

## The CLI it expects

The app depends on three latch commands:
- `latch status --local`
- `latch events --follow [--after CURSOR] [--lifecycle-only]`
- `latch rules show`

The exact JSON shapes are the samples in `Latch/FakeLatchClient.swift` (`Fixtures`). `scripts/stub-latch` serves the same shapes from files.
