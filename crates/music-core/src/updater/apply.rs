//! Replacing installed files with new ones, all or nothing, and the helper mode
//! that does it for the Windows portable zip after the app has exited.
//!
//! [`replace_files`] works in two phases. First every new file is copied next to
//! its target as `<name>.update-new` (the install is untouched; a failure just
//! removes those copies). Then each target is renamed to `<name>.update-old`
//! and the new copy renamed into place; if any rename fails, every step is undone
//! in reverse. Only after all files are in place are the `.update-old` files
//! deleted (left behind if that fails: a running exe on Windows; cleaned up by
//! [`remove_leftovers`] on the next launch).
//!
//! **Helper protocol** (keep it stable: the *new* version's exe runs it):
//! `<new exe> --apply-update <staging dir> <install dir> <pid> <log file> <1|0>`
//! waits for `pid` to exit, copies every file under the staging dir into the
//! install dir as above, starts `<install dir>/<exe name>` again if the last
//! argument is `1`, and exits (0 = applied).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::UpdateError;
use super::log::UpdateLog;

const NEW_SUFFIX: &str = ".update-new";
const OLD_SUFFIX: &str = ".update-old";
pub const HELPER_FLAG: &str = "--apply-update";

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Every file under `root`, relative to it, sorted.
pub fn list_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                walk(root, &path, out)?;
            } else if kind.is_file() {
                out.push(path.strip_prefix(root).unwrap_or(&path).to_owned());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

/// Copies `files` (relative paths) from `from` over the same paths in `to`, all or
/// nothing (see the module docs). Returns what was replaced or added.
pub fn replace_files(
    from: &Path,
    to: &Path,
    files: &[PathBuf],
    log: &UpdateLog,
) -> Result<Vec<PathBuf>, UpdateError> {
    // Phase 1: stage copies next to the targets.
    let mut staged: Vec<PathBuf> = Vec::new();
    let mut created_dirs: Vec<PathBuf> = Vec::new();
    let phase1 = (|| -> Result<(), UpdateError> {
        for rel in files {
            if rel.is_absolute()
                || rel
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(UpdateError::InvalidPackage(format!(
                    "unsafe path {}",
                    rel.display()
                )));
            }
            let target = to.join(rel);
            if let Some(parent) = target.parent()
                && !parent.exists()
            {
                // Every missing level, outermost first (removed innermost first).
                let mut missing: Vec<PathBuf> = parent
                    .ancestors()
                    .take_while(|p| !p.exists())
                    .map(Path::to_owned)
                    .collect();
                missing.reverse();
                std::fs::create_dir_all(parent)?;
                created_dirs.extend(missing);
            }
            let new = with_suffix(&target, NEW_SUFFIX);
            let _ = std::fs::remove_file(&new);
            std::fs::copy(from.join(rel), &new)?;
            staged.push(new);
        }
        super::fail_point("copy")
    })();
    if let Err(e) = phase1 {
        for new in &staged {
            let _ = std::fs::remove_file(new);
        }
        for dir in created_dirs.iter().rev() {
            let _ = std::fs::remove_dir(dir);
        }
        log.line(&format!(
            "copying the new files failed ({e}); nothing was changed"
        ));
        return Err(e);
    }

    // Phase 2: swap, undoing everything on the first failure.
    enum Step {
        Replaced { target: PathBuf, old: PathBuf },
        Added { target: PathBuf },
    }
    let mut done: Vec<Step> = Vec::new();
    let phase2 = (|| -> Result<(), UpdateError> {
        for (rel, new) in files.iter().zip(&staged) {
            let target = to.join(rel);
            if target.exists() {
                let old = with_suffix(&target, OLD_SUFFIX);
                let _ = std::fs::remove_file(&old);
                std::fs::rename(&target, &old)?;
                done.push(Step::Replaced {
                    target: target.clone(),
                    old,
                });
            } else {
                done.push(Step::Added {
                    target: target.clone(),
                });
            }
            super::fail_point("swap")?;
            std::fs::rename(new, &target)?;
        }
        Ok(())
    })();
    if let Err(e) = phase2 {
        for step in done.iter().rev() {
            match step {
                Step::Replaced { target, old } => {
                    let _ = std::fs::remove_file(target);
                    if let Err(undo) = std::fs::rename(old, target) {
                        log.line(&format!(
                            "ROLLBACK FAILED for {}: {undo} (the old file is {})",
                            target.display(),
                            old.display()
                        ));
                    }
                }
                Step::Added { target } => {
                    let _ = std::fs::remove_file(target);
                }
            }
        }
        for new in &staged {
            let _ = std::fs::remove_file(new);
        }
        for dir in created_dirs.iter().rev() {
            let _ = std::fs::remove_dir(dir);
        }
        log.line(&format!(
            "replacing files failed ({e}); rolled back {} file(s)",
            done.len()
        ));
        return Err(e);
    }
    let mut replaced = Vec::new();
    for step in done {
        match step {
            Step::Replaced { target, old } => {
                if let Err(e) = std::fs::remove_file(&old) {
                    log.line(&format!("left {} ({e})", old.display()));
                }
                replaced.push(target);
            }
            Step::Added { target } => replaced.push(target),
        }
    }
    Ok(replaced)
}

/// Deletes `.update-old` / `.update-new` files a previous update left in `dir`
/// (not recursive).
pub fn remove_leftovers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(OLD_SUFFIX) || name.ends_with(NEW_SUFFIX) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Waits until process `pid` has exited (at most `timeout`). On Windows, where std
/// has no way to watch another process, it waits until `exe` can be opened for
/// writing (a running image can't).
pub fn wait_for_exit(pid: u32, exe: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !process_running(pid, exe) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[cfg(unix)]
fn process_running(pid: u32, _exe: &Path) -> bool {
    // 0 would address the whole process group.
    let Ok(pid) = libc::pid_t::try_from(pid).map(|p| p.max(0)) else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_running(_pid: u32, exe: &Path) -> bool {
    exe.exists() && std::fs::OpenOptions::new().append(true).open(exe).is_err()
}

/// Starts `exe` detached from this process (no console, own process group), with
/// the script / screenshot variables removed so a test run isn't repeated.
pub fn spawn_detached(exe: &Path, args: &[&std::ffi::OsStr]) -> std::io::Result<()> {
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    scrub_env(&mut command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.spawn().map(drop)
}

/// Variables a relaunched copy must not inherit.
pub(super) const SCRUBBED_ENV: &[&str] = &[
    "MUSIC_SCRIPT",
    "MUSIC_GPUI_SCREENSHOT",
    "MUSIC_UPDATE_FAIL",
    // AppImage runtime variables of the old image.
    "APPIMAGE",
    "APPDIR",
    "ARGV0",
    "OWD",
];

pub(super) fn scrub_env(command: &mut std::process::Command) {
    for var in SCRUBBED_ENV {
        command.env_remove(var);
    }
}

/// The helper mode: `Some(exit code)` if `args` (without the program name) start
/// with [`HELPER_FLAG`]. Call first thing in `main`.
pub fn run_helper(args: &[std::ffi::OsString]) -> Option<i32> {
    if args.first().is_none_or(|a| a != HELPER_FLAG) {
        return None;
    }
    let [_, staging, target, pid, log, relaunch] = args else {
        eprintln!("usage: {HELPER_FLAG} <staging> <target> <pid> <log> <1|0>");
        return Some(2);
    };
    let (staging, target) = (PathBuf::from(staging), PathBuf::from(target));
    let log = UpdateLog::new(PathBuf::from(log));
    let pid: u32 = pid.to_string_lossy().parse().unwrap_or(0);
    let exe_name = std::env::current_exe()
        .ok()
        .and_then(|e| e.file_name().map(|n| n.to_owned()))
        .unwrap_or_default();
    let installed_exe = target.join(&exe_name);
    log.line(&format!(
        "helper: waiting for pid {pid} to exit, then updating {}",
        target.display()
    ));
    if !wait_for_exit(pid, &installed_exe, Duration::from_secs(60)) {
        log.line("helper: the app is still running after 60 s; trying anyway");
    }
    let code = match list_files(&staging)
        .map_err(UpdateError::from)
        .and_then(|files| replace_files(&staging, &target, &files, &log))
    {
        Ok(files) => {
            log.line(&format!("helper: replaced {} file(s)", files.len()));
            0
        }
        Err(e) => {
            log.line(&format!("helper: update failed, old version kept: {e}"));
            1
        }
    };
    if relaunch == "1" {
        match spawn_detached(&installed_exe, &[]) {
            Ok(()) => log.line(&format!("helper: started {}", installed_exe.display())),
            Err(e) => log.line(&format!("helper: couldn't start the app: {e}")),
        }
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "updater-apply-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(root: &Path, rel: &str) -> Option<String> {
        std::fs::read_to_string(root.join(rel)).ok()
    }

    fn setup(name: &str) -> (Dir, PathBuf, PathBuf, UpdateLog) {
        let dir = Dir::new(name);
        let (new, installed) = (dir.0.join("new"), dir.0.join("installed"));
        write(&new, "tunedesk.exe", "v2");
        write(&new, "LICENSE", "license v2");
        write(&new, "share/icons/app.png", "icon v2");
        write(&installed, "tunedesk.exe", "v1");
        write(&installed, "LICENSE", "license v1");
        write(&installed, "settings-the-user-made.txt", "keep");
        let log = UpdateLog::new(dir.0.join("update.log"));
        (dir, new, installed, log)
    }

    #[test]
    fn replaces_and_adds_files() {
        let (dir, new, installed, log) = setup("ok");
        let files = list_files(&new).unwrap();
        assert_eq!(files.len(), 3);
        let replaced = replace_files(&new, &installed, &files, &log).unwrap();
        assert_eq!(replaced.len(), 3);
        assert_eq!(read(&installed, "tunedesk.exe").as_deref(), Some("v2"));
        assert_eq!(read(&installed, "LICENSE").as_deref(), Some("license v2"));
        assert_eq!(
            read(&installed, "share/icons/app.png").as_deref(),
            Some("icon v2")
        );
        assert_eq!(
            read(&installed, "settings-the-user-made.txt").as_deref(),
            Some("keep")
        );
        // No leftovers.
        let left: Vec<_> = list_files(&installed)
            .unwrap()
            .into_iter()
            .filter(|p| p.to_string_lossy().contains(".update-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        drop(dir);
    }

    #[test]
    fn a_failed_copy_changes_nothing() {
        let (dir, new, installed, log) = setup("copy");
        let mut files = list_files(&new).unwrap();
        files.push("missing.dll".into());
        assert!(replace_files(&new, &installed, &files, &log).is_err());
        assert_eq!(read(&installed, "tunedesk.exe").as_deref(), Some("v1"));
        assert_eq!(read(&installed, "LICENSE").as_deref(), Some("license v1"));
        assert!(!installed.join("share").exists());
        assert_eq!(list_files(&installed).unwrap().len(), 3);
        drop(dir);
    }

    #[test]
    fn a_failed_swap_is_rolled_back() {
        let (dir, new, installed, log) = setup("swap");
        let files = list_files(&new).unwrap();
        // Make the last rename fail: a directory where the new copy should go.
        // (`share/icons/app.png.update-new` can't be created over a directory.)
        std::fs::create_dir_all(installed.join("share/icons/app.png.update-new/x")).unwrap();
        assert!(replace_files(&new, &installed, &files, &log).is_err());
        assert_eq!(read(&installed, "tunedesk.exe").as_deref(), Some("v1"));
        assert_eq!(read(&installed, "LICENSE").as_deref(), Some("license v1"));

        // A failure in the middle of the swap phase (after files were moved aside).
        std::fs::remove_dir_all(installed.join("share")).unwrap();
        let swap_fail =
            super::super::with_fail_point("swap", || replace_files(&new, &installed, &files, &log));
        assert!(swap_fail.is_err());
        assert_eq!(read(&installed, "tunedesk.exe").as_deref(), Some("v1"));
        assert_eq!(read(&installed, "LICENSE").as_deref(), Some("license v1"));
        assert!(!installed.join("share/icons/app.png").exists());
        let names: Vec<_> = list_files(&installed).unwrap();
        assert_eq!(names.len(), 3, "{names:?}");
        let log_text = std::fs::read_to_string(dir.0.join("update.log")).unwrap();
        assert!(log_text.contains("rolled back"), "{log_text}");
        drop(dir);
    }

    #[test]
    fn rejects_unsafe_paths() {
        let (dir, new, installed, log) = setup("unsafe");
        assert!(replace_files(&new, &installed, &["../evil".into()], &log).is_err());
        drop(dir);
    }

    #[test]
    fn helper_mode_applies_and_reports() {
        let (dir, new, installed, _) = setup("helper");
        let log = dir.0.join("helper.log");
        let args: Vec<std::ffi::OsString> = vec![
            HELPER_FLAG.into(),
            new.clone().into(),
            installed.clone().into(),
            // A pid that has exited: this test's own children don't exist; 0 is never
            // a user process.
            "0".into(),
            log.clone().into(),
            "0".into(),
        ];
        assert_eq!(run_helper(&args), Some(0));
        assert_eq!(read(&installed, "tunedesk.exe").as_deref(), Some("v2"));
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("replaced 3 file(s)")
        );
        assert_eq!(run_helper(&["--other".into()]), None);
        assert_eq!(run_helper(&[HELPER_FLAG.into()]), Some(2));
        let leftover = installed.join("x.update-old");
        std::fs::write(&leftover, "").unwrap();
        remove_leftovers(&installed);
        assert!(!leftover.exists());
        drop(dir);
    }

    #[test]
    fn waits_for_processes() {
        // This process is running.
        assert!(
            !wait_for_exit(
                std::process::id(),
                Path::new("/nonexistent"),
                Duration::from_millis(200)
            ) || cfg!(not(unix))
        );
        #[cfg(unix)]
        {
            let mut child = std::process::Command::new("sleep")
                .arg("0.3")
                .spawn()
                .unwrap();
            let pid = child.id();
            let waiter = std::thread::spawn(move || {
                wait_for_exit(pid, Path::new("/nonexistent"), Duration::from_secs(5))
            });
            child.wait().unwrap();
            assert!(waiter.join().unwrap());
        }
    }
}
