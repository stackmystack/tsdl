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

lint: clippy fmt-check

setup: setup-hooks
  cargo install git-cliff nextest typos-cli

setup-hooks:
  @mkdir -p .git/hooks
  @cp scripts/post-commit.sh .git/hooks/post-commit
  @echo "Post-commit hook installed successfully."

test *args:
  cargo nextest run {{args}}
