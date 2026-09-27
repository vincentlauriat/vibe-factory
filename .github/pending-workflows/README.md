# Workflows waiting to be installed

The session that prepared 0.2 could not push to `.github/workflows/` (its GitHub token
lacks the `workflow` scope). Move these files into place from a checkout with the right
permissions:

```sh
git mv -f .github/pending-workflows/ci.yml .github/workflows/ci.yml
git mv .github/pending-workflows/release.yml .github/workflows/release.yml
git rm .github/pending-workflows/README.md
git commit -m "ci: install release workflow and MSRV job"
```

* `ci.yml` is the current CI plus an `msrv` job checking the workspace with Rust 1.88.
* `release.yml` runs on a `vX.Y.Z` tag: fmt, clippy and tests on Linux, macOS and Windows,
  a Rust 1.88 check, a tag/crate version and CHANGELOG check, then builds `vibe` for Linux
  x86_64, macOS arm64 and x86_64, Windows x86_64 and publishes them with `SHA256SUMS` and
  the CHANGELOG section as release notes.
