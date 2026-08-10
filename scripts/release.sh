#!/usr/bin/env bash
set -euo pipefail

version=${1:?"Usage: $0 vX.Y.Z"}
date=$(date +%Y-%m-%d)
version_bare=${version#v}

echo "Preparing $version..."

# Update Cargo.toml version.
msg="# managed by release.sh"
sed -E "s/^version = .*[[:space:]]+$msg$/version = \"${version_bare}\" $msg/" \
    Cargo.toml > Cargo.toml.tmp
mv Cargo.toml.tmp Cargo.toml

# Lint and test.
just lint
just test

# Move [Unreleased] content under a new version heading in CHANGELOG.md.
# If [Unreleased] is empty, no section is created.
awk -v ver="$version_bare" -v date="$date" '
/^## \[Unreleased\]/ {
    header = $0
    in_unreleased = 1
    next
}
in_unreleased && /^## \[/ {
    print header
    print ""
    if (content != "") {
        printf "## [%s] - %s\n\n", ver, date
        printf "%s", content
    }
    in_unreleased = 0
    print
    next
}
in_unreleased {
    if (!started && $0 ~ /^[[:space:]]*$/) next
    started = 1
    content = content $0 "\n"
    next
}
{ print }
END {
    if (in_unreleased) {
        print header
        print ""
        if (content != "") {
            printf "## [%s] - %s\n\n", ver, date
            printf "%s", content
        }
    }
}
' CHANGELOG.md > CHANGELOG.md.tmp
mv CHANGELOG.md.tmp CHANGELOG.md

# Commit.
git add -A
git commit -m "chore(release): $version"

# Extract the new section for the tag message.
tag_body=$(scripts/changelog.sh "$version_bare")

git tag -a "$version" -m "Release $version" -m "$tag_body"

git show

echo "Done!"
echo "Now push the commit and tag."
