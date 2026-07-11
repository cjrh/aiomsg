# Coverage support tooling

The scripts here convert language-native reports to the repository's LCOV
upload format. They are intentionally small adapters: coverage percentages are
not comparable across implementations with different native instrumentation.

## Zig / kcov

`zig_kcov_lcov.sh` accepts the Cobertura layouts exercised by
`tests/test_kcov_helpers.sh`:

- `cobertura.xml`, `cov.xml`, or `coverage.xml` directly in a requested kcov
  output directory;
- the same files under a hashed child directory; and
- files under `kcov-merged/` as a fallback.

The final merged report must contain at least one `LF:` source line. The script
fails locally before upload when it does not, so a remote coverage service is
not the first diagnostic. `test_cobertura_to_lcov.py` also covers kcov's
single-file `<source>` plus basename-only class-filename convention.

Run the support-tool fixtures from the repository root:

```sh
just test-tools
```
