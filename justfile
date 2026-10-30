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

# The docs site for plow.co/latch/docs: docs/site/commands.md from this checkout's `latch help --json`, then every page as HTML into OUT (a plow checkout's plow.co/latch/docs).
docs-site out:
    cd latch && cargo build --quiet --locked
    t="${CARGO_TARGET_DIR:-target}"; case "$t" in /*) ;; *) t="latch/$t" ;; esac; python3 scripts/docs_site.py write --latch "$t/debug/latch" --out "{{out}}"

# Renders the docs site into a plow checkout (default: a fresh clone of plow-pbc/plow) and opens a plow PR on branch latch-docs-SHA; --dry-run prints the diff stat instead.
[positional-arguments]
docs-publish *args:
    cd latch && cargo build --quiet --locked
    t="${CARGO_TARGET_DIR:-target}"; case "$t" in /*) ;; *) t="{{justfile_directory()}}/latch/$t" ;; esac; cd "{{invocation_directory()}}" && python3 "{{justfile_directory()}}/scripts/docs_site.py" publish --latch "$t/debug/latch" "$@"

# This checkout as a dev Mac app (.dev/app/Plow Latch Dev.app), on `just build`'s latch, with gog, the Messages reader and Chrome for Testing.
app *args:
    @just --justfile latch/justfile app {{args}}
