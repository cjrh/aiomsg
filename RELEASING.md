# Releasing aiomsg

This repo uses one semver tag (`vX.Y.Z`) for a coherent repo release. Package manifests that need registry publishing are bumped to the same version before the tag is created.

Currently versioned and published:

- `browser-lib/package.json` → npm package `aiomsg-browser`

## One-time npm setup

The first npm publish may need to be done manually so the package exists on npm:

```sh
cd browser-lib
npm publish --access public
```

After the package exists, configure npm trusted publishing for `aiomsg-browser`:

- npm user/package owner: `cjrh`
- Provider: GitHub Actions
- GitHub owner/repository: `cjrh` / `aiomsg`
- Workflow filename: `release.yml`
- Environment name: `npm`
- Allowed action: `npm publish`

The `release.yml` workflow uses GitHub OIDC (`id-token: write`) and `npm publish --provenance --access public`; no `NPM_TOKEN` secret is needed for normal releases after trusted publishing is configured.

## Normal release

From a clean tracked working tree:

```sh
just release patch
just release minor
just release major
```

The release command:

1. verifies all versioned package manifests currently agree;
2. computes the next semver version;
3. checks that the `vX.Y.Z` tag does not exist locally or on `origin`;
4. runs the browser package tests;
5. bumps `browser-lib/package.json`;
6. checks `npm pack --dry-run`;
7. commits the version bump;
8. creates an annotated `vX.Y.Z` tag;
9. atomically pushes the branch and tag to `origin`.

Pushing the tag starts `.github/workflows/release.yml`, which verifies that the tag matches `browser-lib/package.json`, reruns tests, skips publishing if that exact package version is already on npm, and otherwise publishes `aiomsg-browser`.

## Exact-version tagging

`just release <x.y.z>` is also supported. This is useful for tagging an already-published current version without creating a new version commit:

```sh
just release 1.0.0
```

Exact versions must be equal to or newer than the current package version.

## Adding future published packages

When another implementation gains registry publishing:

1. add its manifest path to `VERSIONED_FILES` in `tools/release.mjs`;
2. add its publish job to `.github/workflows/release.yml`;
3. document any one-time registry/trusted-publisher setup here.
