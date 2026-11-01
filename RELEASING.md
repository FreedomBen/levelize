# Releasing levelize

The release checklist. CI builds and attaches binaries when the tag lands;
publishing to crates.io is a manual step at the end.

## Checklist

- [ ] Working tree clean, `main` up to date.
- [ ] Version: `Cargo.toml` `version` is the release version; `CHANGELOG.md`
      has a dated section for it (replace "unreleased" with today's date).
      `/bump-version vX.Y.Z` (project skill) sets the version everywhere.
- [ ] Verify locally:
  - [ ] `make lint` (fmt + clippy, all features)
  - [ ] `make test` (full suite, all features)
  - [ ] `make man` and spot-check: `man -l doc/levelize.1`
- [ ] Package sanity:
  - [ ] `cargo package --list` — nothing missing, nothing that shouldn't ship
  - [ ] `cargo publish --dry-run`
- [ ] Commit the release prep, then tag and push:
  - [ ] `git tag -a vX.Y.Z -m "levelize X.Y.Z"`
  - [ ] `git push origin main vX.Y.Z`
- [ ] Watch the Release workflow: it builds Linux (musl x86_64/aarch64),
      macOS (universal), and Windows archives, packages the Linux binaries
      as `.deb`/`.rpm` (amd64/arm64), checksums everything, and creates
      the GitHub release. (A `workflow_dispatch` run builds the same
      artifacts without creating a release, as a dry run.)
- [ ] Publish to crates.io: `cargo publish` (needs `cargo login` once).
- [ ] Verify `cargo install levelize` works from a clean machine or
      container.
- [ ] Announce — the release's draft in `docs/` (e.g.
      [docs/announce-v0.1.md](docs/announce-v0.1.md)).
- [ ] Start the next development cycle: bump `Cargo.toml` to the next
      `-dev`-ish minor and add an "unreleased" CHANGELOG section.
