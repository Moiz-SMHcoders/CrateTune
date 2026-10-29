//! Putting a verified package in place, per install kind, and starting the new
//! version once this process has exited.
//!
//! Everything compiles on every OS (the tools are only called for the matching
//! [`InstallKind`]), so one platform's CI type-checks the others' paths.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::apply::{self, HELPER_FLAG};
use super::kind::InstallKind;
use super::log::UpdateLog;
use super::sums::sha256_file;
use super::version::Version;
use super::{Staged, UpdateError, Updater, fail_point};

/// Inno Setup flags for an unattended update: no wizard, no message boxes, no
/// reboot, no "This will install…" prompt, no Cancel button. `/RELAUNCH=1` is read
/// by `packaging/windows/installer.iss` (`[Run]` entry for silent updates).
pub const INNO_SILENT_FLAGS: &[&str] = &[
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/SP-",
    "/NOCANCEL",
];

pub(super) fn install(
    updater: &Updater,
    staged: &Staged,
    relaunch: bool,
) -> Result<(), UpdateError> {
    let log = &updater.log;
    match &updater.kind {
        InstallKind::MacBundle { bundle } => {
            install_mac_bundle(bundle, &staged.file, &staged.version, log)?;
            if relaunch {
                relaunch_after_exit(Path::new("/usr/bin/open"), &mac_open_args(bundle), log)?;
            }
        }
        InstallKind::AppImage { path } => {
            install_appimage(path, staged, log)?;
            if relaunch {
                relaunch_after_exit(path, &[], log)?;
            }
        }
        InstallKind::LinuxTarball { prefix, exe } => {
            let work = updater.version_dir(&staged.version).join("unpacked");
            install_tarball(prefix, &updater.app_id, &staged.file, &work, log)?;
            if relaunch {
                relaunch_after_exit(exe, &[], log)?;
            }
        }
        InstallKind::WindowsInstaller { .. } => {
            let mut args: Vec<OsString> = INNO_SILENT_FLAGS.iter().map(OsString::from).collect();
            let mut log_arg = OsString::from("/LOG=");
            log_arg.push(updater.dir.join("installer.log"));
            args.push(log_arg);
            if relaunch {
                args.push("/RELAUNCH=1".into());
            }
            fail_point("launch")?;
            let args: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
            apply::spawn_detached(&staged.file, &args)
                .map_err(|e| UpdateError::Install(format!("couldn't start the installer: {e}")))?;
            log.line(&format!(
                "started {} silently; it replaces the app once it has exited",
                staged.file.display()
            ));
        }
        InstallKind::WindowsPortable { exe } => {
            let staging = updater.version_dir(&staged.version).join("portable");
            let _ = std::fs::remove_dir_all(&staging);
            std::fs::create_dir_all(&staging)?;
            extract_zip(&staged.file, &staging)?;
            let name = exe.file_name().unwrap_or_default();
            let new_exe = staging.join(name);
            if !new_exe.is_file() {
                return Err(UpdateError::InvalidPackage(format!(
                    "the zip has no {}",
                    name.to_string_lossy()
                )));
            }
            let target = exe.parent().unwrap_or(Path::new("."));
            fail_point("launch")?;
            let pid = std::process::id().to_string();
            let args: [&OsStr; 6] = [
                OsStr::new(HELPER_FLAG),
                staging.as_os_str(),
                target.as_os_str(),
                OsStr::new(&pid),
                log.path().as_os_str(),
                OsStr::new(if relaunch { "1" } else { "0" }),
            ];
            apply::spawn_detached(&new_exe, &args)
                .map_err(|e| UpdateError::Install(format!("couldn't start the helper: {e}")))?;
            log.line(&format!(
                "started the update helper {}; it replaces {} once the app has exited",
                new_exe.display(),
                target.display()
            ));
        }
        InstallKind::Manual(reason) => return Err(UpdateError::Manual(reason.clone())),
    }
    Ok(())
}

/// Runs a tool; `Err` is its name, exit status and stderr.
fn run_tool(program: &str, args: &[&OsStr]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(format!(
            "{program} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn run(program: &str, args: &[&OsStr]) -> Result<String, UpdateError> {
    run_tool(program, args).map_err(UpdateError::Install)
}

// ---- macOS ----------------------------------------------------------------------

/// `open -n [--env MUSIC_…=…] <bundle>`: LaunchServices doesn't pass this process's
/// environment on, so the app's own `MUSIC_*` settings (another config dir, the
/// file secret store, …) are handed over explicitly; nothing is added for a
/// normal launch.
fn mac_open_args(bundle: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-n".into()];
    for (key, value) in std::env::vars_os() {
        let Some(name) = key.to_str() else { continue };
        if name.starts_with("MUSIC_") && !apply::SCRUBBED_ENV.contains(&name) {
            let mut pair = key.clone();
            pair.push("=");
            pair.push(&value);
            args.push("--env".into());
            args.push(pair);
        }
    }
    args.push(bundle.as_os_str().to_owned());
    args
}

fn plist_value(bundle: &Path, key: &str) -> Option<String> {
    let plist = bundle.join("Contents/Info.plist");
    run_tool(
        "/usr/libexec/PlistBuddy",
        &[
            OsStr::new("-c"),
            OsStr::new(&format!("Print :{key}")),
            plist.as_os_str(),
        ],
    )
    .ok()
    .filter(|v| !v.is_empty())
}

/// Unpacks the `.app.zip` next to the bundle, checks it, swaps it in.
fn install_mac_bundle(
    bundle: &Path,
    zip: &Path,
    expected: &Version,
    log: &UpdateLog,
) -> Result<(), UpdateError> {
    let parent = bundle
        .parent()
        .ok_or_else(|| UpdateError::Install("the bundle has no folder".into()))?;
    let name = bundle.file_name().unwrap_or_default().to_string_lossy();
    let work = parent.join(format!(".{name}.update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let result = (|| {
        run(
            "/usr/bin/ditto",
            &[
                OsStr::new("-x"),
                OsStr::new("-k"),
                zip.as_os_str(),
                work.as_os_str(),
            ],
        )?;
        let new_app = single_app(&work)?;
        verify_mac_bundle(&new_app, bundle, expected, log)?;
        swap_bundle(bundle, &new_app, log)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn single_app(dir: &Path) -> Result<PathBuf, UpdateError> {
    let apps: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().is_some_and(|e| e == "app"))
        .collect();
    match apps.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(UpdateError::InvalidPackage(format!(
            "expected one .app in the archive, found {}",
            apps.len()
        ))),
    }
}

/// The signature is intact, it is the same app (bundle id) and the version the
/// release promised.
fn verify_mac_bundle(
    new_app: &Path,
    current: &Path,
    expected: &Version,
    log: &UpdateLog,
) -> Result<(), UpdateError> {
    run_tool(
        "/usr/bin/codesign",
        &[
            OsStr::new("--verify"),
            OsStr::new("--strict"),
            new_app.as_os_str(),
        ],
    )
    .map_err(|e| UpdateError::InvalidPackage(format!("code signature: {e}")))?;
    fail_point("verify")?;
    let new_id = plist_value(new_app, "CFBundleIdentifier");
    let current_id = plist_value(current, "CFBundleIdentifier");
    if current_id.is_some() && new_id != current_id {
        return Err(UpdateError::InvalidPackage(format!(
            "bundle id {new_id:?}, expected {current_id:?}"
        )));
    }
    let version = plist_value(new_app, "CFBundleShortVersionString");
    if version.as_deref().and_then(Version::parse).as_ref() != Some(expected) {
        return Err(UpdateError::InvalidPackage(format!(
            "the package is version {version:?}, expected {expected}"
        )));
    }
    let exe = plist_value(new_app, "CFBundleExecutable").unwrap_or_default();
    if exe.is_empty() || !new_app.join("Contents/MacOS").join(&exe).is_file() {
        return Err(UpdateError::InvalidPackage("no executable".into()));
    }
    log.line(&format!(
        "verified {} (codesign --verify --strict, {}, {expected})",
        new_app.display(),
        new_id.unwrap_or_default()
    ));
    Ok(())
}

/// Old bundle aside, new one in, old one deleted; the old one is put back if the
/// new one can't be moved in.
fn swap_bundle(bundle: &Path, new_app: &Path, log: &UpdateLog) -> Result<(), UpdateError> {
    let parent = bundle.parent().unwrap_or(Path::new("."));
    let name = bundle.file_name().unwrap_or_default().to_string_lossy();
    let old = parent.join(format!(".{name}.old-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(bundle, &old)
        .map_err(|e| UpdateError::Install(format!("couldn't move the old app aside: {e}")))?;
    if let Err(e) = fail_point("swap").and_then(|()| Ok(std::fs::rename(new_app, bundle)?)) {
        return match std::fs::rename(&old, bundle) {
            Ok(()) => {
                log.line(&format!("swap failed ({e}); the old app is back in place"));
                Err(e)
            }
            Err(undo) => {
                log.line(&format!(
                    "ROLLBACK FAILED: {undo}; the old app is at {}",
                    old.display()
                ));
                Err(UpdateError::Install(format!(
                    "{e}; the old app is at {}",
                    old.display()
                )))
            }
        };
    }
    log.line(&format!("replaced {}", bundle.display()));
    if let Err(e) = std::fs::remove_dir_all(&old) {
        log.line(&format!("couldn't delete {} ({e})", old.display()));
    }
    Ok(())
}

/// Leftovers of an interrupted macOS update next to `bundle`.
pub(super) fn remove_mac_leftovers(bundle: &Path) {
    let (Some(parent), Some(name)) = (bundle.parent(), bundle.file_name()) else {
        return;
    };
    let name = name.to_string_lossy();
    let prefixes = [format!(".{name}.update-"), format!(".{name}.old-")];
    for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
        let file = entry.file_name();
        let file = file.to_string_lossy();
        if prefixes.iter().any(|p| file.starts_with(p.as_str())) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

// ---- Linux ----------------------------------------------------------------------

/// Copies the AppImage next to the old one, checks the copy's hash again, makes it
/// executable and renames it over the old one (atomic; the running image keeps
/// its open file).
fn install_appimage(path: &Path, staged: &Staged, log: &UpdateLog) -> Result<(), UpdateError> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.update-new"));
    let result = (|| {
        std::fs::copy(&staged.file, &tmp)?;
        let actual = sha256_file(&tmp)?;
        if actual != staged.sha256 {
            return Err(UpdateError::ChecksumMismatch {
                file: tmp.display().to_string(),
                expected: staged.sha256.clone(),
                actual,
            });
        }
        set_executable(&tmp)?;
        fail_point("swap")?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    match &result {
        Ok(()) => log.line(&format!("replaced {}", path.display())),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            log.line(&format!(
                "AppImage not replaced ({e}); the old one is untouched"
            ));
        }
    }
    result
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Unpacks the tarball and replaces `bin/` and `share/` under `prefix` with its
/// files (all or nothing).
fn install_tarball(
    prefix: &Path,
    app_id: &str,
    tarball: &Path,
    work: &Path,
    log: &UpdateLog,
) -> Result<(), UpdateError> {
    let _ = std::fs::remove_dir_all(work);
    std::fs::create_dir_all(work)?;
    let result = (|| {
        run(
            "tar",
            &[
                OsStr::new("-xzf"),
                tarball.as_os_str(),
                OsStr::new("-C"),
                work.as_os_str(),
            ],
        )?;
        let top = tarball_root(work, app_id)?;
        let files = tarball_files(&top, app_id)?;
        let replaced = apply::replace_files(&top, prefix, &files, log)?;
        log.line(&format!(
            "replaced {} file(s) under {}",
            replaced.len(),
            prefix.display()
        ));
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(work);
    result
}

/// The tarball's one top directory (`<id>-<ver>-linux-<arch>/`).
fn tarball_root(work: &Path, app_id: &str) -> Result<PathBuf, UpdateError> {
    let dirs: Vec<PathBuf> = std::fs::read_dir(work)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    match dirs.as_slice() {
        [one] if one.join("bin").join(app_id).is_file() => Ok(one.clone()),
        _ => Err(UpdateError::InvalidPackage(format!(
            "no {app_id}-…/bin/{app_id} in the tarball"
        ))),
    }
}

/// What `install.sh` copies: everything under `bin/` and `share/`.
fn tarball_files(top: &Path, app_id: &str) -> Result<Vec<PathBuf>, UpdateError> {
    let files: Vec<PathBuf> = apply::list_files(top)?
        .into_iter()
        .filter(|p| p.starts_with("bin") || p.starts_with("share"))
        .collect();
    if !files.iter().any(|p| *p == Path::new("bin").join(app_id)) {
        return Err(UpdateError::InvalidPackage(format!("no bin/{app_id}")));
    }
    Ok(files)
}

// ---- Windows (portable) ---------------------------------------------------------

/// Unpacks a zip into `dir` (entries that would land outside it are refused).
pub(super) fn extract_zip(zip: &Path, dir: &Path) -> Result<(), UpdateError> {
    let bad = |e: &dyn std::fmt::Display| UpdateError::InvalidPackage(format!("zip: {e}"));
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?).map_err(|e| bad(&e))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| bad(&e))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(bad(&"an entry points outside the archive"));
        };
        let out = dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut file)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o755));
        }
    }
    Ok(())
}

// ---- Relaunch -------------------------------------------------------------------

/// Waits (up to 30 s) for this process to exit, then runs the program. `$1` is the
/// pid, the rest the command; nothing is interpolated into the script.
const WAIT_THEN_EXEC: &str = r#"pid="$1"; shift; i=0
while kill -0 "$pid" 2>/dev/null; do i=$((i+1)); [ "$i" -ge 300 ] && break; sleep 0.1; done
exec "$@""#;

/// Starts a detached `sh` that runs `program args…` once this process has exited.
fn relaunch_after_exit(
    program: &Path,
    args: &[OsString],
    log: &UpdateLog,
) -> Result<(), UpdateError> {
    let pid = std::process::id().to_string();
    let mut all: Vec<&OsStr> = vec![
        OsStr::new("-c"),
        OsStr::new(WAIT_THEN_EXEC),
        OsStr::new("sh"),
        OsStr::new(&pid),
        program.as_os_str(),
    ];
    all.extend(args.iter().map(OsString::as_os_str));
    apply::spawn_detached(Path::new("/bin/sh"), &all)
        .map_err(|e| UpdateError::Install(format!("couldn't schedule the restart: {e}")))?;
    log.line(&format!(
        "will start {} {} after pid {pid} exits",
        program.display(),
        args.iter()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("updater-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_the_tarball_tree() {
        let work = temp("tarball");
        let top = work.join("tunedesk-0.2.0-linux-x86_64");
        for rel in [
            "bin/tunedesk",
            "share/applications/tunedesk.desktop",
            "share/icons/hicolor/64x64/apps/tunedesk.png",
            "install.sh",
            "LICENSE",
        ] {
            let p = top.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, rel).unwrap();
        }
        assert_eq!(tarball_root(&work, "tunedesk").unwrap(), top);
        let files = tarball_files(&top, "tunedesk").unwrap();
        assert_eq!(files.len(), 3);
        assert!(!files.iter().any(|f| f.ends_with("install.sh")));
        assert!(tarball_root(&work, "other").is_err());
        std::fs::remove_dir_all(&work).unwrap();
    }

    #[test]
    fn extracts_zips_safely() {
        use std::io::Write as _;
        let dir = temp("zip");
        let zip_path = dir.join("p.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("tunedesk.exe", options).unwrap();
            zip.write_all(b"exe").unwrap();
            zip.start_file("sub/LICENSE", options).unwrap();
            zip.write_all(b"gpl").unwrap();
            zip.finish().unwrap();
        }
        let out = dir.join("out");
        extract_zip(&zip_path, &out).unwrap();
        assert_eq!(std::fs::read(out.join("tunedesk.exe")).unwrap(), b"exe");
        assert_eq!(std::fs::read(out.join("sub/LICENSE")).unwrap(), b"gpl");

        let evil = dir.join("evil.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&evil).unwrap());
            zip.start_file("../escape.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        }
        assert!(extract_zip(&evil, &dir.join("out2")).is_err());
        assert!(!dir.join("escape.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_args_pass_the_apps_variables_only() {
        let args = mac_open_args(Path::new("/Applications/TuneDesk.app"));
        assert_eq!(args.first().map(|a| a.as_os_str()), Some(OsStr::new("-n")));
        assert_eq!(
            args.last().map(|a| a.as_os_str()),
            Some(OsStr::new("/Applications/TuneDesk.app"))
        );
        for pair in args.chunks(2).skip(1) {
            if pair.len() == 2 && pair[0] == "--env" {
                let s = pair[1].to_string_lossy();
                assert!(
                    s.starts_with("MUSIC_") && !s.starts_with("MUSIC_SCRIPT="),
                    "{s}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn appimage_swap_is_atomic_and_checked() {
        let dir = temp("appimage");
        let installed = dir.join("TuneDesk.AppImage");
        std::fs::write(&installed, b"old").unwrap();
        let file = dir.join("new.AppImage");
        std::fs::write(&file, b"new").unwrap();
        let log = UpdateLog::new(dir.join("update.log"));
        let mut staged = Staged {
            version: Version::new(9, 0, 0),
            file,
            sha256: "0".repeat(64),
        };
        // Wrong hash: refused, old one untouched.
        assert!(matches!(
            install_appimage(&installed, &staged, &log),
            Err(UpdateError::ChecksumMismatch { .. })
        ));
        assert_eq!(std::fs::read(&installed).unwrap(), b"old");
        staged.sha256 = sha256_file(&staged.file).unwrap();
        // A failure right before the rename: still the old one.
        assert!(
            super::super::with_fail_point("swap", || install_appimage(&installed, &staged, &log))
                .is_err()
        );
        assert_eq!(std::fs::read(&installed).unwrap(), b"old");
        install_appimage(&installed, &staged, &log).unwrap();
        assert_eq!(std::fs::read(&installed).unwrap(), b"new");
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("update-new"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_swap_rolls_back() {
        let dir = temp("macswap");
        let bundle = dir.join("TuneDesk.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(bundle.join("Contents/marker"), b"old").unwrap();
        let new_app = dir.join("work/TuneDesk.app");
        std::fs::create_dir_all(new_app.join("Contents")).unwrap();
        std::fs::write(new_app.join("Contents/marker"), b"new").unwrap();
        let log = UpdateLog::new(dir.join("update.log"));
        assert!(
            super::super::with_fail_point("swap", || swap_bundle(&bundle, &new_app, &log)).is_err()
        );
        assert_eq!(
            std::fs::read(bundle.join("Contents/marker")).unwrap(),
            b"old"
        );
        swap_bundle(&bundle, &new_app, &log).unwrap();
        assert_eq!(
            std::fs::read(bundle.join("Contents/marker")).unwrap(),
            b"new"
        );
        // The old bundle was deleted.
        let hidden = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".TuneDesk.app"))
            .count();
        assert_eq!(hidden, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
