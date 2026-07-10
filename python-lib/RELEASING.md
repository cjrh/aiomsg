# Python releases

The Python package is part of the repository-wide aiomsg release. Do not create
a Python-only commit or tag from this directory. From a clean, current repository
`master` checkout, run:

```sh
just release 2026.7.1
```

The root release command updates this package's `pyproject.toml` and `uv.lock`,
updates every other implementation to the same version, tests the repository,
and creates the repository tag `v2026.7.1`. That tag triggers this package's
PyPI publication after the workflow verifies the tag and package version agree.

## One-time PyPI setup

PyPI publishing uses trusted publishing (OIDC), so no API token is stored in
this repository. Configure the `aiomsg` project's trusted publisher with:

- owner: `cjrh`
- repository: `aiomsg`
- workflow filename: `release.yml`
- environment: `pypi`

Also create the `pypi` environment in the GitHub repository. See the
[PyPI trusted-publisher documentation](https://docs.pypi.org/trusted-publishers/)
for the current setup flow.
