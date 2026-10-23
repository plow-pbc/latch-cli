# latch's front door. The CLI's other recipes are in latch/justfile.

set dotenv-filename := ".env.local"

# Builds this checkout for release into .dev/bin/latch, for hand testing.
build *args:
    @just --justfile latch/justfile build {{args}}

# Runs test suites (default: this OS's gate); --linux runs them in the Linux container.
[positional-arguments]
test *args:
    @python3 scripts/test.py "$@"

# Lists the suites: what each covers and needs, and where it runs.
test-list:
    @python3 scripts/test.py --list

# Prints what `just test ARGS` would run, without running it.
[positional-arguments]
test-plan *args:
    @python3 scripts/test.py --dry-run "$@"

# Asks the real decision model through Plow (not tests): checks, replay OUT, scenario.
gatekeeper *args:
    @just --justfile latch/justfile gatekeeper {{args}}

# This checkout as a dev Mac app (.dev/app/Plow Latch Dev.app), on `just build`'s latch, with gog, the Messages reader and Chrome for Testing.
app *args:
    @just --justfile latch/justfile app {{args}}
