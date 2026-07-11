# Releasing aiomsg

aiomsg has one coherent release version: every version-bearing implementation is
set to `X.Y.Z`, and the release commit receives the annotated tag `vX.Y.Z`.
Each implementation owns the mechanics for updating its native metadata; the
root `justfile` owns testing, the one commit, and the tags.

## Normal release

Release only from a clean, current `master` checkout:

```sh
just release 2026.7.1
```

The command:

1. checks that `master` is clean and matches `origin/master`;
2. invokes `set-release-version` in each implementation;
3. runs all implementation tests and checks the browser npm package contents;
4. commits any pending version changes as `Release v2026.7.1`;
5. creates annotated `v2026.7.1` and `golang-lib/v2026.7.1` tags; and
6. atomically pushes `master` and both tags.

The Go module needs its directory-prefixed tag for `go get`; it points at the
same commit as the repository release tag. LuaRocks records the same release as
`2026.7.1-1`, where `-1` is the rock revision. Go has no version field in
`go.mod`; its tag is its version.

If the final push is rejected, no remote ref has changed because it is atomic.
The local release commit and tags remain for inspection; after resolving the
rejection, either push them explicitly or reset them before trying again.

## Release inventory and metadata contract

[`tools/release_inventory.py`](tools/release_inventory.py) is the authoritative
machine-readable inventory for the coherent repository release. It records each
participating implementation, its version source, registry, and publication
eligibility; the root `set-release-version` recipe reads its package list rather
than maintaining a second list.

Inspect the inventory with `python3 tools/release_inventory.py list`. Validate a
checkout without mutating it or contacting a registry with:

```sh
just check-release-metadata 2026.7.1
```

Every inventory entry has a package-local `just set-release-version VERSION`
recipe. Go is the deliberate exception: its module version is the
`golang-lib/vX.Y.Z` tag rather than a manifest field. LuaRocks is likewise
special: the rockspec filename and its version use `X.Y.Z-1`, where `-1` is the
rock revision. The metadata check verifies both Rust manifest/lockfile pairs,
the LuaRocks filename/content pair, and every other version-bearing manifest
before a release can commit or tag.

## Published packages

Pushing `vX.Y.Z` runs `.github/workflows/release.yml`. The workflow verifies
that the tag matches both publishable manifests, then publishes:

- `python-lib` package `aiomsg` to PyPI;
- `browser-lib` package `aiomsg-browser` to npm.

Other implementations still receive matching build/package metadata, but are
not published by this workflow yet.

## One-time registry setup

PyPI uses its existing `pypi` GitHub environment and trusted publisher. For
npm, create the `npm` GitHub environment and configure npm trusted publishing
for `aiomsg-browser` with owner `cjrh`, repository `aiomsg`, workflow
`release.yml`, and environment `npm`. The npm job uses OIDC and provenance, so
no long-lived npm token is required.

## Tag trust boundary

A pushed `vX.Y.Z` tag is a production publication command: it can publish both
PyPI `aiomsg` and npm `aiomsg-browser`. Create and push such tags only through
`just release` after its clean-tree, current-`master`, metadata, test, and
package-content checks succeed. Repository administrators must also protect the
`v*` tag namespace and restrict tag creation to authorized maintainers; trusted
publisher environments authenticate the workflow, but cannot compensate for an
unauthorized tag push.

The clean-worktree preflight rejects both tracked and untracked changes. The
ignored `.benchmarks/` directory is reserved for reproducible benchmark output;
keep any other generated artifacts out of the checkout or remove them before
starting a release.
