# The CLI's recipes are in latch/justfile; these run from here too.

build *args:
    @just --justfile latch/justfile build {{args}}

test:
    @just --justfile latch/justfile test
