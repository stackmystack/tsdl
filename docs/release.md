# Creating a Release

[GitHub](https://github.com/stackmystack/tsdl/releases) and
[crates.io](https://crates.io/crates/tsdl/)
releases are automated via
[GitHub actions](./.github/workflows/release.yml)
and triggered by pushing a tag.

1. Run the [release script](./release.sh): `scripts/release.sh`.
   The current version will be computed automatically if no version `v[X.Y.Z]` was passed.
2. Push the changes: `git push`
3. Check if [Continuous Integration](https://github.com/stackmystack/tsdl/actions)
   workflow is completed successfully.
4. Push the tags: `git push --tags`
5. Wait for [Release](https://github.com/stackmystack/tsdl/actions)
   workflow to finish.
