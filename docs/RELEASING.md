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
7. **Package managers:** update the Homebrew tap, the Scoop bucket, the AUR and Flathub from
   the release's `cratetune-X.Y.Z-package-manifests.tar.gz` (see
   [Package managers](#package-managers)), and refresh the copies in `packaging/` with
   `scripts/update-packages.sh --in-place` in the next commit.

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
| `cratetune-X.Y.Z-package-manifests.tar.gz` | the Homebrew cask, Scoop manifest, AUR PKGBUILDs and Flatpak manifest for this version ([Package managers](#package-managers)); not used by the updater |
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

## Package managers

CrateTune is published through four package managers. Each one installs the release's own
packages (checked against `SHA256SUMS.txt`), and each one tells the app it is in charge, so
the in-app updater stays off and Settings → About says "Updates come from <manager>"
(`music_core::updater::PackageManager`).

| Channel | Install | Manifest in this repo | Published in | The app knows it by |
|---|---|---|---|---|
| Homebrew | `brew install --cask moiz-smhcoders/tap/cratetune` | `packaging/homebrew/cratetune.rb` | tap repo `Moiz-SMHcoders/homebrew-tap`, `Casks/cratetune.rb` | a `Caskroom/cratetune` folder |
| Scoop | `scoop bucket add cratetune https://github.com/Moiz-SMHcoders/scoop-bucket` then `scoop install cratetune` | `packaging/scoop/cratetune.json` | bucket repo `Moiz-SMHcoders/scoop-bucket`, `bucket/cratetune.json` | the `scoop\apps\cratetune` path and a `packaged` marker the manifest writes |
| AUR | `yay -S cratetune-bin` (or `cratetune`, built from source) | `packaging/aur/cratetune-bin/`, `packaging/aur/cratetune/` | `ssh://aur@aur.archlinux.org/cratetune-bin.git` (and `cratetune.git`) | `/usr/share/cratetune/packaged` (`aur`) |
| Flatpak | `flatpak install flathub io.github.moiz_smhcoders.cratetune` | `packaging/flatpak/` | `github.com/flathub/io.github.moiz_smhcoders.cratetune` (made by Flathub) | `FLATPAK_ID`, and `CRATETUNE_PACKAGED=flatpak` |

Any other package can do the same with the environment variable `CRATETUNE_PACKAGED=<name>`
(set by its launcher) or a one-word `packaged` file: `<prefix>/share/cratetune/packaged` on
Linux, next to `cratetune.exe` on Windows, `CrateTune.app/Contents/Resources/packaged` on
macOS. `flatpak`, `snap`, `aur`, `homebrew` and `scoop` get their proper names; any other word
is shown as written.

### How the manifests are made

`scripts/update-packages.sh` writes all of them from a release's `SHA256SUMS.txt` (names, URLs
and ids come from `packaging/app.env`):

```sh
scripts/update-packages.sh --version X.Y.Z               # → target/packages/
scripts/update-packages.sh --version X.Y.Z --in-place    # → packaging/ (then review and commit)
```

It downloads `SHA256SUMS.txt` from the published release unless `--sums FILE` is given, and
for the source PKGBUILD it downloads and hashes the tag's archive (`--no-source` skips it).

In `release.yml`, the `packages` job runs it on the built assets, validates the result
(`appstreamcli validate`, `desktop-file-validate`, `makepkg --printsrcinfo` against the
committed `.SRCINFO` in an Arch container, JSON and Ruby syntax), and the release gets the
result as `cratetune-X.Y.Z-package-manifests.tar.gz`. The `flatpak` job builds the Flatpak from
the Linux tarball with `org.flatpak.Builder`, runs Flathub's linter (reported, not enforced)
and keeps the bundle as a workflow artifact. **Nothing is pushed**: the workflow doesn't
commit to `main` or touch the tap, bucket, AUR or Flathub, because a bad automated push to
any of them reaches users at once. Publishing them is the maintainer's step below; it can be
automated later with a deploy key per repository.

To check the jobs without a release: `gh workflow run release.yml --ref <branch>` (builds
everything, publishes nothing).

### Each release

After publishing the GitHub release, download and unpack
`cratetune-X.Y.Z-package-manifests.tar.gz`, then:

* **Homebrew:** copy `homebrew/cratetune.rb` to `Casks/cratetune.rb` in the tap, commit,
  push. (`brew livecheck --cask moiz-smhcoders/tap/cratetune` shows when it is behind.)
* **Scoop:** copy `scoop/cratetune.json` to `bucket/cratetune.json`, commit, push. With the
  bucket's Excavator workflow (below), this happens on its own: `checkver` finds the release
  and `autoupdate` takes the hash from `SHA256SUMS.txt`.
* **AUR:** in each AUR clone, copy `PKGBUILD` and `.SRCINFO` from `aur/<pkgname>/`, then
  `makepkg -si` once on Arch to try it, `git commit -am "Update to X.Y.Z" && git push`.
* **Flathub:** Flathub's bot reads the `x-checker-data` in the manifest and opens a pull
  request on the Flathub repository when a release appears; check that its build passes and
  merge it. Without the bot, copy the three files from `flatpak/` there.
* Run `scripts/update-packages.sh --version X.Y.Z --in-place` here and commit the refreshed
  `packaging/` with the next change.

### One-time setup

**Homebrew tap**

1. Create the public repository `Moiz-SMHcoders/homebrew-tap` (the `homebrew-` prefix makes
   it `moiz-smhcoders/tap`).
2. Add `Casks/cratetune.rb` from `packaging/homebrew/` and a README with the install command.
3. Check it on a Mac: `brew install --cask moiz-smhcoders/tap/cratetune`,
   `brew audit --cask --strict --online moiz-smhcoders/tap/cratetune`,
   `brew style moiz-smhcoders/tap`.
4. Optional: `brew tap-new` adds GitHub Actions that test pull requests to the tap.

The app is ad-hoc signed, not notarized, so the cask clears the quarantine flag in
`postflight_steps` after Homebrew has verified the DMG's sha256 (otherwise Gatekeeper refuses
to open it), and its `caveats` say so. That is fine in a personal tap; the main
`homebrew/cask` repository only accepts notarized apps, so moving there needs a Developer ID
certificate and notarization first (`MACOS_SIGN_IDENTITY`, see [Signing](#signing-macos)).
Once notarized, drop the `postflight_steps` block.

**Scoop bucket**

1. Create the repository from the template <https://github.com/ScoopInstaller/BucketTemplate>
   as `Moiz-SMHcoders/scoop-bucket`.
2. Add `bucket/cratetune.json` from `packaging/scoop/`.
3. Keep its Excavator workflow (it runs `checkver` + `autoupdate` on a schedule and commits
   new versions) and give it permission to push.
4. Check it on Windows: `scoop bucket add cratetune https://github.com/Moiz-SMHcoders/scoop-bucket`,
   `scoop install cratetune`, then Settings → About shows "Updates come from Scoop".

Later, the manifest can be proposed to the `extras` bucket (needs some popularity).

**AUR**

1. Create an account on <https://aur.archlinux.org> and add an SSH public key to it.
2. For each package (`cratetune-bin`, and `cratetune` if wanted):
   ```sh
   git clone ssh://aur@aur.archlinux.org/cratetune-bin.git   # empty: creates the package
   cp packaging/aur/cratetune-bin/{PKGBUILD,.SRCINFO} cratetune-bin/
   cd cratetune-bin && makepkg -si && namcap PKGBUILD *.pkg.tar.zst
   git add PKGBUILD .SRCINFO && git commit -m "Initial import: X.Y.Z" && git push
   ```
3. Put your name and e-mail in the `# Maintainer:` line (the script uses the GitHub owner),
   or change `MAINTAINER` in `scripts/update-packages.sh`.

The `-bin` package installs the release tarball (built on Ubuntu 22.04, so any current Arch
runs it). The source package builds with Arch's `cargo` (stable Rust) and
`scripts/bundle-linux.sh --no-build`; it must keep up with the `rust-toolchain.toml` version.
`.SRCINFO` is written by the script in `makepkg --printsrcinfo`'s format, and CI compares the
two; on Arch, `makepkg --printsrcinfo > .SRCINFO` gives the same file.

**Flathub**

Don't submit anything until these are true:

* The app id is `io.github.moiz_smhcoders.cratetune` (Flathub's rule for GitHub projects:
  `io.github.<owner>.<repo>`, `-` → `_`). After the app is accepted, verify it on
  flathub.org by logging in with the GitHub account that owns the repository.
* The AppStream file (`packaging/linux/app.metainfo.xml.in`, rendered into
  `packaging/flatpak/*.metainfo.xml`) has current screenshots: Flathub mirrors the ones
  listed, so check that each picture shows the current app. Add a `<release>` description for
  each version if you want release notes in software centres.
* The `flatpak` job of `release.yml` passed and its lint report has no errors other than the
  screenshot-mirror ones (those only pass on Flathub's builders).

Submitting (<https://docs.flathub.org/docs/for-app-authors/submission>):

1. Fork `github.com/flathub/flathub`, branch from `new-pr`, add
   `io.github.moiz_smhcoders.cratetune.yml`, `.metainfo.xml` and `.desktop` from
   `packaging/flatpak/`, and open a pull request against `new-pr`.
2. Answer the review. Expect questions about the permissions, each of which is commented in
   the manifest: `--talk-name=org.freedesktop.secrets` (the keyring; the app uses the Secret
   Service directly, not the portal), `--own-name=org.mpris.MediaPlayer2.cratetune`, the Discord
   IPC socket, Downloads and Music.
3. Flathub then creates `github.com/flathub/io.github.moiz_smhcoders.cratetune`; later
   releases go there (Flathub's bot opens the pull requests from `x-checker-data`).

**Why the Flatpak repackages the release tarball instead of compiling.** The tarball is the
binary CI builds with the pinned toolchain (`rust-toolchain.toml`) on Ubuntu 22.04 and tests;
the Flatpak runs exactly that, its manifest stays small, and building it takes seconds
instead of compiling ~1,000 crates. The freedesktop 25.08 runtime has every library the binary
loads (Vulkan loader and Mesa drivers, Wayland, X11, xkbcommon, ALSA through PipeWire). The
cost: x86_64 only (no aarch64 tarball yet), and Flathub prefers source builds of open-source
apps, so reviewers may ask for one. Then switch the module to a source build: add the
`org.freedesktop.Sdk.Extension.rust-stable` SDK extension, replace the archive with a `git`
source (tag + commit), generate `cargo-sources.json` with
[flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)
from `Cargo.lock` (no git dependencies, so it works as is), and build with
`cargo --offline build --release -p music-gpui`, then install the same files as now. The SDK
extension's Rust must be at least the `rust-toolchain.toml` version.

**Snap** is detected too (`SNAP` set: "Updates come from Snap") but there is no snap package.
