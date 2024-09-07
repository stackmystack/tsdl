#!/usr/bin/env -S just --justfile

clippy:
  cargo clippy --all --all-targets -- --deny warnings

clippy-fix *args:
  cargo clippy --fix {{args}}

clippy-fix-now:
  @just clippy-fix --allow-dirty --allow-staged

fmt:
  cargo fmt --all

fmt-check:
  cargo fmt --all -- --check

lint: clippy fmt-check typos

setup:
  cargo install git-cliff nextest typos-cli

test *args:
  cargo nextest run {{args}}

typos:
  typos --sort

typos-fix:
  typos --write-changes
