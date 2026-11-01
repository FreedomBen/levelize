---
name: bump-version
description: Bump this project's release version to the version given as the first argument (e.g. /bump-version v0.0.2). Updates Cargo.toml, refreshes Cargo.lock, updates the CHANGELOG section (checking git tags first, so a released version keeps its own notes), sweeps for stale version references, and verifies the build — without committing. Use whenever the user asks to bump, set, raise, or change the version, or to prepare the next release number.
argument-hint: <version>
---

# Bump the release version

Set the project's version to the one given as the argument, consistently
across every file that carries it, and verify the result. This prepares a
version change; it never commits, tags, or publishes (see RELEASING.md for
the release steps themselves).

## Parse and validate the argument

Accept the version with or without a leading `v` (`v0.0.2` and `0.0.2` both
mean the same thing). Strip the `v` for file contents — Cargo and the
CHANGELOG use bare semver like `0.0.2`; the `v` prefix belongs only to git
tags, which this skill does not create.

- No argument: stop and ask which version to bump to.
- Not of the shape `MAJOR.MINOR.PATCH` with an optional `-prerelease`
  suffix (`0.2.0-rc.1` is fine): stop and say why, rather than writing a
  malformed version anywhere.
- Equal to the current version in Cargo.toml: nothing to do — say so.
- Lower than the current version: proceed (retagging a planned release
  downward is legitimate), but point out that it is a downgrade so a typo
  gets caught.

## Apply the change

Work from the repo root. Three files carry the version:

1. **Cargo.toml** — set `version = "<bare>"` under `[package]`.
2. **Cargo.lock** — regenerated, not edited: run `cargo check --quiet`
   after the Cargo.toml edit. That rewrites only this package's lock entry
   and leaves dependency versions alone (never run a bare `cargo update`,
   which would bump dependencies too).
3. **CHANGELOG.md** — make sure the new version has a section. Don't
   trust an `unreleased` marker on the old version's section: the
   RELEASING.md step that dates it can get skipped, so a shipped version
   may still say "unreleased" (this exact case happened with 0.0.1).
   Check `git tag -l "v<old>"` first, then:
   - A `## [<bare>]` section already exists → leave the file alone.
   - The top section is the old version, marked `unreleased`, and **no
     `v<old>` tag exists** → retitle that heading to
     `## [<bare>] — unreleased` (the pending notes simply get the new
     number).
   - The top section is the old version but **`v<old>` is tagged** → it
     shipped. Give its heading the release date
     (`gh release view v<old> --json publishedAt`, else the tagged
     commit's date) and keep under it only what the tag actually
     contains (compare `git show v<old>:CHANGELOG.md`); move work from
     after the tag (`git log v<old>..HEAD`) into a new
     `## [<bare>] — unreleased` section above it, filing behavior
     changes to existing features under `### Changed`, not `### Added`.
   - Otherwise (top section is a dated, released version) → insert a new
     `## [<bare>] — unreleased` section above it, with a placeholder line
     for notes.

## Sweep for stale references

Search the repo for the old version string outside the three files above
(skip `target/`). Judge what turns up instead of blind-replacing:

- Illustrative text — the tag examples in RELEASING.md, a version-named
  announcement draft like `docs/announce-v0.1.md` — stays as it is; it
  describes a specific release, not the current version.
- Anything that genuinely tracks the current version should be updated and
  is worth mentioning, since it means a new file started embedding the
  version and this skill's list above needs extending.

Code and tests read the version via `env!("CARGO_PKG_VERSION")`, so they
follow Cargo.toml automatically.

## Verify and report

- `cargo check --quiet` passes (already run for the lock refresh).
- `cargo run --quiet -- --version` prints the new version.
- Show a short summary: old → new version, the files changed
  (`git diff --stat`), and anything the sweep flagged.

Leave the changes uncommitted — committing happens only when the user asks,
per this repo's conventions.
