# latch's front door. The CLI's other recipes are in latch/justfile.

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

# Runs the real decision model (not tests): checks (default), replay OUT, scenario.
[positional-arguments]
gatekeeper what="checks" *args:
    #!/bin/sh
    set -eu
    # The key file: LATCH_TEST_JEV_KEY, never a flag.
    key="${LATCH_TEST_JEV_KEY:?set LATCH_TEST_JEV_KEY to the jev key file}"
    cd latch
    case "$1" in
      checks) just test-model "$key" && just gatekeeper-live "$key" && just gatekeeper-owner "$key" ;;
      replay) [ "$#" -eq 2 ] || { echo "usage: just gatekeeper replay OUT" >&2; exit 2; }
              just gatekeeper-replay "$key" "$2" ;;
      scenario) just scenario-return "$key" ;;
      *) echo "just gatekeeper [checks | replay OUT | scenario]" >&2; exit 2 ;;
    esac
