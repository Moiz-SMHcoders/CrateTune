//! How the running copy was installed, which release asset updates it, and
//! whether it may update itself in place.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Windows,
    Linux,
    Other,
}

impl Os {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            Os::Other
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Arm64,
    X86_64,
    Other,
}

impl Arch {
    /// The architecture this binary was built for (an x64 build under emulation on
    /// an ARM machine updates to the x64 package).
    pub const fn current() -> Self {
        if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else {
            Arch::Other
        }
    }
}

/// Why a copy can only show a download link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualReason {
    /// Not a packaged install: `cargo run`, a bare binary.
    DevBuild,
    /// macOS runs the app from a read-only randomized path (App Translocation:
    /// a quarantined app started from where it was downloaded). Moving it to
    /// Applications fixes it.
    Translocated,
    /// Installed by a package manager, Flatpak or Snap (or system-wide, read-only).
    SystemPackage,
    /// No package for this OS / architecture.
    UnsupportedPlatform,
}

/// The install the updater replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// `…/CrateTune.app` (updated from `<id>-<ver>-macos-<arch>.app.zip`).
    MacBundle { bundle: PathBuf },
    /// Inno Setup per-user install (`unins000.exe` next to the exe); updated by
    /// running the new `-setup.exe` silently.
    WindowsInstaller { exe: PathBuf },
    /// The portable zip, unpacked anywhere; updated by the helper mode
    /// (`--apply-update`) of the new exe.
    WindowsPortable { exe: PathBuf },
    /// `$APPIMAGE`; the new AppImage is renamed over it.
    AppImage { path: PathBuf },
    /// The tarball's `install.sh` tree: `<prefix>/bin/<id>` +
    /// `<prefix>/share/applications/<id>.desktop`.
    LinuxTarball { prefix: PathBuf, exe: PathBuf },
    /// Only a download link.
    Manual(ManualReason),
}

/// What [`InstallKind::detect`] looks at (all of it fakeable for tests).
pub struct Environment<'a> {
    pub os: Os,
    pub app_id: &'a str,
    /// The running executable (canonical path).
    pub exe: &'a Path,
    /// `$APPIMAGE` (set by the AppImage runtime).
    pub appimage: Option<&'a Path>,
    /// Running inside Flatpak (`FLATPAK_ID`) or Snap (`SNAP`).
    pub sandboxed: bool,
    pub exists: &'a dyn Fn(&Path) -> bool,
}

impl InstallKind {
    pub fn detect(env: &Environment) -> Self {
        let exe = env.exe;
        let file_name = exe.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let dir = exe.parent().unwrap_or(Path::new(""));
        match env.os {
            Os::MacOs => {
                let contents = dir.parent();
                let bundle = contents.and_then(Path::parent);
                let is_bundle = dir.file_name().is_some_and(|n| n == "MacOS")
                    && contents
                        .and_then(Path::file_name)
                        .is_some_and(|n| n == "Contents")
                    && bundle
                        .and_then(|b| b.extension())
                        .is_some_and(|e| e == "app");
                match bundle {
                    Some(_)
                        if is_bundle && exe.to_string_lossy().contains("/AppTranslocation/") =>
                    {
                        InstallKind::Manual(ManualReason::Translocated)
                    }
                    Some(bundle) if is_bundle => InstallKind::MacBundle {
                        bundle: bundle.to_owned(),
                    },
                    _ => InstallKind::Manual(ManualReason::DevBuild),
                }
            }
            Os::Windows => {
                // Packages name the exe `<id>.exe`; `cargo build` makes `music-gpui.exe`.
                if !file_name.eq_ignore_ascii_case(&format!("{}.exe", env.app_id)) {
                    InstallKind::Manual(ManualReason::DevBuild)
                } else if (env.exists)(&dir.join("unins000.exe")) {
                    InstallKind::WindowsInstaller {
                        exe: exe.to_owned(),
                    }
                } else {
                    InstallKind::WindowsPortable {
                        exe: exe.to_owned(),
                    }
                }
            }
            Os::Linux => {
                if env.sandboxed {
                    return InstallKind::Manual(ManualReason::SystemPackage);
                }
                if let Some(path) = env.appimage {
                    return InstallKind::AppImage {
                        path: path.to_owned(),
                    };
                }
                let prefix = dir
                    .parent()
                    .filter(|_| dir.file_name().is_some_and(|n| n == "bin"));
                match prefix {
                    Some(prefix)
                        if file_name == env.app_id
                            && !is_system_prefix(prefix)
                            && (env.exists)(
                                &prefix
                                    .join("share/applications")
                                    .join(format!("{}.desktop", env.app_id)),
                            ) =>
                    {
                        InstallKind::LinuxTarball {
                            prefix: prefix.to_owned(),
                            exe: exe.to_owned(),
                        }
                    }
                    Some(prefix) if file_name == env.app_id && is_system_prefix(prefix) => {
                        InstallKind::Manual(ManualReason::SystemPackage)
                    }
                    _ => InstallKind::Manual(ManualReason::DevBuild),
                }
            }
            Os::Other => InstallKind::Manual(ManualReason::UnsupportedPlatform),
        }
    }

    /// Detects the running process's install.
    pub fn detect_current(app_id: &str) -> Self {
        let Ok(exe) = std::env::current_exe() else {
            return InstallKind::Manual(ManualReason::DevBuild);
        };
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        let appimage = std::env::var_os("APPIMAGE")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from);
        let sandboxed =
            std::env::var_os("FLATPAK_ID").is_some() || std::env::var_os("SNAP").is_some();
        InstallKind::detect(&Environment {
            os: Os::current(),
            app_id,
            exe: &exe,
            appimage: appimage.as_deref(),
            sandboxed,
            exists: &|p: &Path| p.exists(),
        })
    }

    /// The end of the asset's name (after `<id>-<version>`) for this install, or
    /// `None` if there is no package for it.
    pub fn asset_suffix(&self, arch: Arch) -> Option<&'static str> {
        Some(match (self, arch) {
            (InstallKind::MacBundle { .. }, Arch::Arm64) => "-macos-arm64.app.zip",
            (InstallKind::MacBundle { .. }, Arch::X86_64) => "-macos-x86_64.app.zip",
            (InstallKind::WindowsInstaller { .. }, Arch::X86_64) => "-windows-x64-setup.exe",
            (InstallKind::WindowsPortable { .. }, Arch::X86_64) => "-windows-x64.zip",
            (InstallKind::AppImage { .. }, Arch::X86_64) => "-x86_64.AppImage",
            (InstallKind::AppImage { .. }, Arch::Arm64) => "-aarch64.AppImage",
            (InstallKind::LinuxTarball { .. }, Arch::X86_64) => "-linux-x86_64.tar.gz",
            (InstallKind::LinuxTarball { .. }, Arch::Arm64) => "-linux-aarch64.tar.gz",
            _ => return None,
        })
    }

    pub fn can_self_update(&self) -> bool {
        !matches!(self, InstallKind::Manual(_))
    }

    /// Short English name (logs, `@update-status`).
    pub fn name(&self) -> &'static str {
        match self {
            InstallKind::MacBundle { .. } => "macos-bundle",
            InstallKind::WindowsInstaller { .. } => "windows-installer",
            InstallKind::WindowsPortable { .. } => "windows-portable",
            InstallKind::AppImage { .. } => "appimage",
            InstallKind::LinuxTarball { .. } => "linux-tarball",
            InstallKind::Manual(ManualReason::DevBuild) => "unpackaged",
            InstallKind::Manual(ManualReason::Translocated) => "translocated",
            InstallKind::Manual(ManualReason::SystemPackage) => "system-package",
            InstallKind::Manual(ManualReason::UnsupportedPlatform) => "unsupported",
        }
    }

    /// The installed files' location (the bundle, exe, AppImage or prefix).
    pub fn path(&self) -> Option<&Path> {
        match self {
            InstallKind::MacBundle { bundle } => Some(bundle),
            InstallKind::WindowsInstaller { exe } | InstallKind::WindowsPortable { exe } => {
                Some(exe)
            }
            InstallKind::AppImage { path } => Some(path),
            InstallKind::LinuxTarball { prefix, .. } => Some(prefix),
            InstallKind::Manual(_) => None,
        }
    }

    /// Directories the update has to write to (checked before downloading).
    pub fn writable_dirs(&self) -> Vec<PathBuf> {
        let parent = |p: &Path| p.parent().map(Path::to_owned);
        match self {
            // Renaming the bundle needs its folder; deleting the old one, the bundle.
            InstallKind::MacBundle { bundle } => parent(bundle)
                .into_iter()
                .chain([bundle.join("Contents")])
                .collect(),
            InstallKind::WindowsInstaller { exe }
            | InstallKind::WindowsPortable { exe }
            | InstallKind::AppImage { path: exe } => parent(exe).into_iter().collect(),
            InstallKind::LinuxTarball { prefix, .. } => {
                let share = prefix.join("share");
                std::iter::once(prefix.join("bin"))
                    .chain(share.exists().then_some(share))
                    .collect()
            }
            InstallKind::Manual(_) => Vec::new(),
        }
    }

    /// The first directory in [`Self::writable_dirs`] this process can't write to.
    pub fn unwritable_dir(&self) -> Option<PathBuf> {
        self.writable_dirs().into_iter().find(|d| !dir_writable(d))
    }
}

/// `/usr`, `/opt`, … : where packages and admins install (never updated in place).
fn is_system_prefix(prefix: &Path) -> bool {
    prefix == Path::new("/usr") || prefix.starts_with("/opt") || prefix.starts_with("/snap")
}

/// Creates and removes a probe file in `dir`.
pub fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".update-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(os: Os, exe: &str, appimage: Option<&str>, files: &[&str]) -> InstallKind {
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        let exists = move |p: &Path| files.iter().any(|f| f == p);
        InstallKind::detect(&Environment {
            os,
            app_id: "cratetune",
            exe: Path::new(exe),
            appimage: appimage.map(Path::new),
            sandboxed: false,
            exists: &exists,
        })
    }

    #[test]
    fn detects_macos_bundles() {
        assert_eq!(
            detect(
                Os::MacOs,
                "/Applications/CrateTune.app/Contents/MacOS/music-gpui",
                None,
                &[]
            ),
            InstallKind::MacBundle {
                bundle: "/Applications/CrateTune.app".into()
            }
        );
        assert_eq!(
            detect(
                Os::MacOs,
                "/Users/me/src/target/release/music-gpui",
                None,
                &[]
            ),
            InstallKind::Manual(ManualReason::DevBuild)
        );
        assert_eq!(
            detect(
                Os::MacOs,
                "/private/var/folders/x/AppTranslocation/ABC/d/CrateTune.app/Contents/MacOS/music-gpui",
                None,
                &[]
            ),
            InstallKind::Manual(ManualReason::Translocated)
        );
    }

    #[test]
    fn detects_windows_installs() {
        let installed = r"C:\Users\me\AppData\Local\Programs\CrateTune\cratetune.exe";
        // Path::parent only understands `\` on Windows; use `/` for the fake paths.
        let installed_fwd = installed.replace('\\', "/");
        let uninstaller = installed_fwd.replace("cratetune.exe", "unins000.exe");
        assert_eq!(
            detect(Os::Windows, &installed_fwd, None, &[&uninstaller]),
            InstallKind::WindowsInstaller {
                exe: installed_fwd.clone().into()
            }
        );
        assert_eq!(
            detect(
                Os::Windows,
                "D:/Portable/CrateTune/CrateTune.EXE",
                None,
                &[]
            ),
            InstallKind::WindowsPortable {
                exe: "D:/Portable/CrateTune/CrateTune.EXE".into()
            }
        );
        assert_eq!(
            detect(
                Os::Windows,
                "C:/src/target/release/music-gpui.exe",
                None,
                &[]
            ),
            InstallKind::Manual(ManualReason::DevBuild)
        );
    }

    #[test]
    fn detects_linux_installs() {
        assert_eq!(
            detect(
                Os::Linux,
                "/tmp/.mount_tuneXYZ/usr/bin/cratetune",
                Some("/home/me/Apps/cratetune-0.1.0-x86_64.AppImage"),
                &[]
            ),
            InstallKind::AppImage {
                path: "/home/me/Apps/cratetune-0.1.0-x86_64.AppImage".into()
            }
        );
        assert_eq!(
            detect(
                Os::Linux,
                "/home/me/.local/bin/cratetune",
                None,
                &["/home/me/.local/share/applications/cratetune.desktop"]
            ),
            InstallKind::LinuxTarball {
                prefix: "/home/me/.local".into(),
                exe: "/home/me/.local/bin/cratetune".into()
            }
        );
        // No desktop file: not the tarball layout.
        assert_eq!(
            detect(Os::Linux, "/home/me/.local/bin/cratetune", None, &[]),
            InstallKind::Manual(ManualReason::DevBuild)
        );
        assert_eq!(
            detect(
                Os::Linux,
                "/usr/bin/cratetune",
                None,
                &["/usr/share/applications/cratetune.desktop"]
            ),
            InstallKind::Manual(ManualReason::SystemPackage)
        );
        let sandboxed = InstallKind::detect(&Environment {
            os: Os::Linux,
            app_id: "cratetune",
            exe: Path::new("/app/bin/cratetune"),
            appimage: None,
            sandboxed: true,
            exists: &|_| true,
        });
        assert_eq!(sandboxed, InstallKind::Manual(ManualReason::SystemPackage));
    }

    #[test]
    fn asset_suffixes() {
        let mac = InstallKind::MacBundle {
            bundle: "/A.app".into(),
        };
        assert_eq!(mac.asset_suffix(Arch::Arm64), Some("-macos-arm64.app.zip"));
        assert_eq!(
            mac.asset_suffix(Arch::X86_64),
            Some("-macos-x86_64.app.zip")
        );
        let setup = InstallKind::WindowsInstaller { exe: "x".into() };
        assert_eq!(setup.asset_suffix(Arch::Arm64), None);
        assert_eq!(
            InstallKind::Manual(ManualReason::DevBuild).asset_suffix(Arch::X86_64),
            None
        );
    }

    #[test]
    fn probes_writability() {
        let dir = std::env::temp_dir().join(format!("updater-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(dir_writable(&dir));
        assert!(!dir_writable(&dir.join("missing")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
