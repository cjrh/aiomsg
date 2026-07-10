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
