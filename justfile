# The CLI's recipes are in latch/justfile; these run from here too.

set dotenv-filename := ".env.local"

build *args:
    @just --justfile latch/justfile build {{args}}

test:
    @just --justfile latch/justfile test
