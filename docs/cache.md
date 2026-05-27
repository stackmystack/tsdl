# Cache Semantics

TSDL caches parser builds in `<build-dir>/cache.toml`. The cache is a build
reuse decision, not only a list of produced files. A cache entry is reused only
when the parser source, build inputs, and expected artifacts still match the
current request.

## Cache Entries

Each grammar is cached independently under a `language/grammar` key. For
example, the JSON parser is cached as `json/json`, while a repository with
multiple grammars can produce keys such as `typescript/typescript` and
`typescript/tsx`.

Each entry records:

- the `grammar.js` content hash;
- the parser repository URL;
- the requested parser git ref;
- the parser source cache identity;
- the resolved tree-sitter CLI release tag, platform, and repository;
- the build script, output prefix, and target;
- the expected build artifacts on disk.

If any of these values no longer match, the grammar is rebuilt.

## Parser Git Refs

Parser refs are first interpreted from the user definition in `parsers.toml` or
from the default unpinned build behavior.

Stable parser refs are cached by the requested ref only:

- full 40-character commit SHAs;
- dotted versions such as `0.21.0`, normalized to `v0.21.0`;
- `v` plus dotted versions such as `v0.21.0`;
- explicit tag refs under `refs/tags/...`.

Moving parser refs are cached by both the requested ref and the resolved commit:

- `HEAD`;
- branches such as `master`, `main`, or `dev`;
- explicit branch refs under `refs/heads/...`;
- other unqualified names such as `release`, `stable`, or `vnext`.

The requested ref always remains part of the cache identity. If `main` and
`stable` point to the same commit, they are still different cache identities.

## Moving Ref Behavior

Moving refs are resolved before cache lookup. TSDL checks out the requested ref,
reads the resulting `HEAD` commit, and compares that commit with the cached
commit for the same requested ref.

This means:

- if a branch or `HEAD` has not moved, the cache can be reused;
- if a branch or `HEAD` has moved, the affected grammars are rebuilt;
- if the requested ref changes, the cache misses even when both refs currently
  point to the same commit.

Tag-like parser refs are not remotely resolved for cache purposes. A tag ref is
treated as stable by name. If a remote tag is moved, TSDL does not detect that
movement from the cache alone; use `--force` or `--fresh` when you need to force
a rebuild.

## Tree-Sitter CLI Identity

The tree-sitter CLI version is resolved to the actual release tag that exists in
the configured tree-sitter repository. The cache records that resolved release
tag rather than the raw input string.

For example, if both `0.26.5` and `v0.26.5` resolve to the same release tag, the
parser cache treats them as the same tree-sitter CLI identity.

Changing the tree-sitter CLI release tag, platform, or repository invalidates
the affected parser cache entries.

## Cache Migration

Older cache files do not contain parser source cache identities.

Stable parser refs can keep using old cache entries because their cache identity
is still the requested ref. Moving parser refs miss once after the cache format
upgrade because TSDL needs to store the resolved commit for future comparisons.
After that rebuild, the cache entry contains the resolved commit and normal
moving-ref cache behavior applies.

Old entries may also miss once when their tree-sitter CLI version was stored as
raw input and the current run resolves it to a canonical release tag.

## Manual Controls

Use `--force` to ignore cache reuse for the current build. Use `--fresh` to clear
the build directory before building. These options override normal cache reuse
and are useful when remote state was intentionally changed outside the semantics
above.
