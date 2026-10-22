# The CLI's recipes are in latch/justfile; `just build` runs from here too.
build *args:
    @just --justfile latch/justfile build {{args}}
