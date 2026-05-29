#!/usr/bin/env bash
set -euo pipefail

query=${1:?"Usage: $0 <version|unreleased>"}

# Strip leading v, lowercase for case-insensitive matching.
query_lower=$(echo "${query#v}" | tr '[:upper:]' '[:lower:]')

awk -v q="$query_lower" '
    /^## \[/ {
        if (found) exit
        idx = index($0, "[")
        end = index($0, "]")
        if (idx > 0 && end > idx) {
            heading = substr($0, idx + 1, end - idx - 1)
            if (tolower(heading) == q) found = 1
        }
    }
    found
' CHANGELOG.md
