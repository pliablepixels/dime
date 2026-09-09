---
name: make-release
description: Cut a DiMe release - pick the version, write the notes, build the universal binary and the app bundle, tag, and publish to GitHub. Use when the user asks to cut, make, or publish a release.
---

# Cut a DiMe release

`make release V=x.y.z` does the mechanical half: bump, tag, push, build both artifacts,
publish. This skill is the half that needs judgement: which version, and what the notes say.

## 1. Check the tree is releasable

```sh
git status --porcelain          # must be empty
git rev-parse --abbrev-ref HEAD # master
git log origin/master..HEAD     # must be empty: release from what is pushed
cargo test --release && cargo build --release 2>&1 | grep -c warning
```

Stop and say so if the tree is dirty, the branch is not master, tests fail, or the build
warns. Do not release around a failure.

## 2. Decide the version

```sh
git describe --tags --abbrev=0            # last release
git log --oneline <last-tag>..HEAD        # what is in this one
```

DiMe is pre-1.0, so the middle number carries the weight:

- **patch** (0.3.1 -> 0.3.2): fixes and polish only. Nothing new to learn.
- **minor** (0.3.1 -> 0.4.0): a new feature, a new panel or tab, a new rule category, a
  changed default, anything that alters `~/.dime` on disk, or a fix a user would go
  looking for. Most releases here.
- **major**: not until 1.0.

State the number and the one-line reason before using it. If the call is genuinely close,
say which way you lean and ask.

## 3. Write the notes

Read every commit in the range, not just the subjects, and write for someone who runs
DiMe and has never read this repo:

```sh
git log <last-tag>..HEAD --format='%h %s%n%b'
```

Save to `dist/notes-v<version>.md` (dist is gitignored, so it does not pollute the tree):

**Bullets, all the way down. No prose paragraphs anywhere in the file.**

- Open with one to three bullets saying what this release is about, under no heading.
- Then `## What's new`, `## Fixed`, `## Under the hood` - only the sections that have
  content, each one nothing but bullets.
- One bullet is one line and one idea: what the user sees, not the code that moved. Lead
  with the thing itself, in bold if it needs a name. Two sentences at the very most, and
  only when the second one carries a measured number.
- Numbers that were actually measured belong here; invented ones never do.
- No emoji, no "we are excited", no feature the release does not contain.
- Do not write a changelog link or a commit list. GitHub generates that and it is appended
  below whatever this file says, so writing one yourself duplicates it.

Show the user the file as written, bullets and all. Do not re-flow it into paragraphs for
the chat: they are approving the text that ships.

Wait for the user's go-ahead before publishing.

## 4. Release

```sh
make release V=<version> NOTES=dist/notes-v<version>.md
```

That bumps `Cargo.toml`, commits, tags, pushes master and the tag, builds the universal
binary and the signed-ad-hoc app bundle, and creates the GitHub release with both
artifacts attached. The body is the summary followed by GitHub's own generated changelog;
without `NOTES` it is the changelog alone, which is not what this skill is for.

If it fails partway, find out how far it got before retrying: the tag and the push may
already exist (`git tag -d` and `git push --delete origin <tag>` undo them), and
`gh release delete` removes a half-made release. `make release` refuses to start on a
dirty tree or an existing tag, so clean up rather than force.

## 5. Verify, then report

```sh
gh release view v<version> --json name,assets,body -q '.name, (.assets[].name), .body'
lipo -info dist/dime-<version>/dime      # must list arm64 and x86_64
```

Both artifacts must be attached: `DiMe-<version>-macos.zip` and `dime-<version>-macos.tar.gz`.
Give the user the release URL and the two artifact names. Say the version and why it was
that number.
