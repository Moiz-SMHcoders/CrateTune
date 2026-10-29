# Releasing

How to cut a release that the in-app updater can install. The updater (`music_core::updater`,
Settings → About → Updates) reads the GitHub releases of `UPDATE_REPO` in
`packaging/app.env` (`Moiz-SMHcoders/CrateTune`).

## Steps

1. **Bump the version** in the root `Cargo.toml` (`[workspace.package] version`). The tag,
   the asset names and the macOS `CFBundleShortVersionString` all come from it. The
   updater refuses a macOS package whose bundle version differs from the tag.
2. **Changelog:** rename `## [Unreleased]` in `CHANGELOG.md` to `## [X.Y.Z] — <date>` and
   start a new empty `## [Unreleased]` above it.
3. Commit, then **tag and push**:
   ```sh
   git tag vX.Y.Z        # or vX.Y.Z-beta.1 for a pre-release
   git push origin main vX.Y.Z
   ```
4. `.github/workflows/release.yml` builds on macOS arm64 + x86_64, Linux x86_64 and
   Windows x64. The `publish` job then:
   * checks that every package below exists for this version (fails otherwise);
   * writes `SHA256SUMS.txt` over all assets (`sha256sum`, the same format as
     `shasum -a 256`) and checks it;
   * creates a **draft** release with all assets and generated notes. Tags with a `-`
     (`-beta.1`, `-rc.1`) are marked as pre-releases; plain `vX.Y.Z` tags are not.
5. **Check the draft** on GitHub: the asset list (below), the notes, the pre-release box.
6. **Publish** the draft. Running apps pick it up at their next check (at launch, then
   every 24 hours, or **Check now**).

Don't edit or re-upload an asset after publishing. Every installed copy that checks
verifies the package against `SHA256SUMS.txt` and refuses it if they don't match. If an
asset is wrong, delete the release and tag, then cut a new patch version.

## Assets

| Asset | Used for |
|---|---|
| `cratetune-X.Y.Z-macos-arm64.dmg`, `…-macos-x86_64.dmg` | first install on macOS |
| `cratetune-X.Y.Z-macos-arm64.app.zip`, `…-macos-x86_64.app.zip` | **macOS updates** (`ditto -c -k --keepParent`, made by `scripts/bundle-macos.sh`) |
| `cratetune-X.Y.Z-windows-x64-setup.exe` | first install and **updates of installed copies** (Inno Setup, run with `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL /RELAUNCH=1`) |
| `cratetune-X.Y.Z-windows-x64.zip` | portable copies: first install and **updates** |
| `cratetune-X.Y.Z-x86_64.AppImage` | AppImage: first install and **updates** |
| `cratetune-X.Y.Z-linux-x86_64.tar.gz` | tarball installs (`install.sh`): first install and **updates** |
| `SHA256SUMS.txt` | **required**: no sums file, or no line for the package, means no update |

The updater looks for `<APP_ID>-<version><suffix>`, so the names must stay as they are.
If you rename `APP_ID`, already installed copies stop finding updates.

## Pre-releases

**Publish the release after 0.1.0 as a normal release (not a pre-release).** 0.1.0 has no
updater: its check reads `releases/latest`, which GitHub answers with the newest release
that isn't a pre-release. A pre-release is invisible to 0.1.0 users, so they would never be
told about the update. v0.1.0 itself is marked as a pre-release, which is fine.

A release counts as a pre-release when GitHub marks it as one or its tag has a `-` part.
Copies running a pre-release or any `0.x` version get pre-releases by default; from 1.0 on,
only users who turn on **Include pre-releases** get them. Drafts are never offered.

## Signing (macOS)

Without `MACOS_SIGN_IDENTITY` the bundle is ad-hoc signed. That works for updates (the
updater checks `codesign --verify --strict` and the bundle id), but every version has a
new code identity. macOS then asks again for keychain access to stored sign-ins and for
the microphone (see the user guide, "Updates"). A Developer ID certificate (secret
`MACOS_SIGN_IDENTITY`, plus notarizing the DMG) keeps the identity stable.

## Testing an update locally (macOS)

`MUSIC_UPDATE_API=http://127.0.0.1:PORT` points the app at a stand-in for the GitHub API.
It must answer `GET /repos/<repo>/releases` and serve the assets from the same origin.
Use these with it:

* `MUSIC_UPDATE_FAKE_VERSION=0.0.1` to pretend an older version is running;
* `MUSIC_UPDATE_FAIL=copy|swap|verify|launch` to fail one install step and check the
  rollback.

The script actions `@update-check`, `@update-status`, `@update-download`,
`@update-install` and `@update-pref=…` drive the flow; see the README, "Self-update".
Always use a copy of the bundle in a temp folder, plus `MUSIC_CONFIG_DIR=<temp>` and
`MUSIC_SECRET_STORE=file`. The relaunch passes the `MUSIC_*` variables on through
`open --env`.
