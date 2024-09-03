#!/usr/bin/env bash

set -eo pipefail

# Avoid recursion by checking an environment variable
if [ "$RUNNING_GIT_POST_COMMIT" != "1" ]
then
  export RUNNING_GIT_POST_COMMIT=1
  git cliff --unreleased --prepend CHANGELOG.md
  git add CHANGELOG.md
  git commit --amend --no-edit
  unset RUNNING_GIT_POST_COMMIT
fi
