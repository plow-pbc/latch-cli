# The CLI's recipes are in latch/justfile; `just try` runs from here too.
try *args:
    @just --justfile latch/justfile try {{args}}
