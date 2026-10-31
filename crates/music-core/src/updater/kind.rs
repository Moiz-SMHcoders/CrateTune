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
    /// Installed system-wide (`/usr`, `/opt`) by a package manager we can't name.
    SystemPackage,
    /// Installed and updated by a known package manager (Flatpak, Homebrew, …):
    /// the self-updater stays off and defers to it.
    PackageManager(PackageManager),
    /// No package for this OS / architecture.
    UnsupportedPlatform,
}

/// A package manager that owns this copy and updates it.
///
/// Found by [`InstallKind::detect`] from, in order: the `<APP_ID>_PACKAGED`
/// environment variable (`CRATETUNE_PACKAGED=flatpak`), the Flatpak / Snap
/// sandbox, a marker file written by the package ([`MARKER_FILE`]), or the
/// install path (a Homebrew Caskroom entry, a Scoop `apps` folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageManager {
    Flatpak,
    Snap,
    /// Arch User Repository (`cratetune-bin`, `cratetune`).
    Aur,
    /// The Homebrew cask.
    Homebrew,
    Scoop,
    /// Any other name a package gave (shown as given).
    Other(String),
}

/// The marker file a package drops to name its manager (one word, e.g. `aur`):
/// Linux `<prefix>/share/<APP_ID>/packaged`, Windows next to the exe, macOS
/// `<bundle>/Contents/Resources/packaged`.
pub const MARKER_FILE: &str = "packaged";

impl PackageManager {
    /// Parses an environment variable or marker file value: `flatpak`, `snap`,
    /// `aur`, `homebrew` (`brew`), `scoop`, or any other short name. Empty, `0`,
    /// `no`, `false` and `none` mean "not packaged".
    pub fn parse(value: &str) -> Option<Self> {
        let word = value.lines().next().unwrap_or_default().trim();
        let lower = word.to_ascii_lowercase();
        Some(match lower.as_str() {
            "" | "0" | "no" | "false" | "none" => return None,
            "flatpak" | "flathub" => PackageManager::Flatpak,
            "snap" => PackageManager::Snap,
            "aur" | "arch" | "pacman" => PackageManager::Aur,
            "homebrew" | "brew" => PackageManager::Homebrew,
            "scoop" => PackageManager::Scoop,
            _ => {
                let name: String = word.chars().filter(|c| !c.is_control()).take(40).collect();
                PackageManager::Other(name)
            }
        })
    }

    /// Lower-case id (logs, `@update-status`).
    pub fn id(&self) -> &str {
        match self {
            PackageManager::Flatpak => "flatpak",
            PackageManager::Snap => "snap",
            PackageManager::Aur => "aur",
            PackageManager::Homebrew => "homebrew",
            PackageManager::Scoop => "scoop",
            PackageManager::Other(name) => name,
        }
    }

    /// The name shown to users ("Updates come from Homebrew").
    pub fn display_name(&self) -> &str {
        match self {
            PackageManager::Flatpak => "Flatpak",
            PackageManager::Snap => "Snap",
            PackageManager::Aur => "AUR",
            PackageManager::Homebrew => "Homebrew",
            PackageManager::Scoop => "Scoop",
            PackageManager::Other(name) => name,
        }
    }

    /// The environment variable that names the manager: `<APP_ID>_PACKAGED`
    /// (`CRATETUNE_PACKAGED`).
    pub fn env_var(app_id: &str) -> String {
        let id: String = app_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("{id}_PACKAGED")
    }
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
    /// `<APP_ID>_PACKAGED` (set by a package's launcher or Flatpak manifest).
    pub packaged: Option<&'a str>,
    /// Running inside Flatpak (`FLATPAK_ID`) or Snap (`SNAP`).
    pub sandbox: Option<PackageManager>,
    pub exists: &'a dyn Fn(&Path) -> bool,
    /// Reads a small text file (the [`MARKER_FILE`]).
    pub read: &'a dyn Fn(&Path) -> Option<String>,
}

impl InstallKind {
    pub fn detect(env: &Environment) -> Self {
        match Self::detect_package_manager(env) {
            Some(manager) => InstallKind::Manual(ManualReason::PackageManager(manager)),
            None => Self::detect_unmanaged(env),
        }
    }

    /// The package manager that owns this copy, if any (see [`PackageManager`]).
    fn detect_package_manager(env: &Environment) -> Option<PackageManager> {
        if let Some(manager) = env.packaged.and_then(PackageManager::parse) {
            return Some(manager);
        }
        if let Some(manager) = &env.sandbox {
            return Some(manager.clone());
        }
        let dir = env.exe.parent().unwrap_or(Path::new(""));
        let marker = match env.os {
            // <bundle>/Contents/MacOS/<exe> → <bundle>/Contents/Resources/packaged
            Os::MacOs => dir.parent().map(|c| c.join("Resources").join(MARKER_FILE)),
            Os::Windows => Some(dir.join(MARKER_FILE)),
            // <prefix>/bin/<exe> → <prefix>/share/<id>/packaged
            Os::Linux => dir
                .parent()
                .map(|p| p.join("share").join(env.app_id).join(MARKER_FILE)),
            Os::Other => None,
        };
        if let Some(manager) = marker
            .and_then(|m| (env.read)(&m))
            .and_then(|v| PackageManager::parse(&v))
        {
            return Some(manager);
        }
        match env.os {
            // The cask moves the app to /Applications and keeps its record in the
            // Caskroom (Apple silicon and Intel prefixes).
            Os::MacOs => (dir.ends_with("Contents/MacOS")
                && ["/opt/homebrew/Caskroom", "/usr/local/Caskroom"]
                    .iter()
                    .any(|room| (env.exists)(&Path::new(room).join(env.app_id))))
            .then_some(PackageManager::Homebrew),
            // `<scoop root>/apps/<id>/<version or current>/<id>.exe`.
            Os::Windows => {
                let parts: Vec<String> = env
                    .exe
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
                    .collect();
                parts
                    .windows(3)
                    .any(|w| w[0] == "scoop" && w[1] == "apps" && w[2] == env.app_id)
                    .then_some(PackageManager::Scoop)
            }
            Os::Linux | Os::Other => None,
        }
    }

    fn detect_unmanaged(env: &Environment) -> Self {
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
        let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        let sandbox = if set("FLATPAK_ID") {
            Some(PackageManager::Flatpak)
        } else if set("SNAP") {
            Some(PackageManager::Snap)
        } else {
            None
        };
        let packaged = std::env::var(PackageManager::env_var(app_id)).ok();
        InstallKind::detect(&Environment {
            os: Os::current(),
            app_id,
            exe: &exe,
            appimage: appimage.as_deref(),
            packaged: packaged.as_deref(),
            sandbox,
            exists: &|p: &Path| p.exists(),
            // A marker is one word; anything big is not ours.
            read: &|p: &Path| {
                let meta = std::fs::metadata(p).ok()?;
                (meta.is_file() && meta.len() < 256)
                    .then(|| std::fs::read_to_string(p).ok())
                    .flatten()
            },
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

    /// The package manager that updates this copy instead of the app.
    pub fn package_manager(&self) -> Option<&PackageManager> {
        match self {
            InstallKind::Manual(ManualReason::PackageManager(manager)) => Some(manager),
            _ => None,
        }
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
            InstallKind::Manual(ManualReason::PackageManager(_)) => "package-manager",
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
            packaged: None,
            sandbox: None,
            exists: &exists,
            read: &|_| None,
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
    }

    /// [`detect`] with a package environment variable, a sandbox and marker files.
    fn detect_packaged(
        os: Os,
        exe: &str,
        packaged: Option<&str>,
        sandbox: Option<PackageManager>,
        files: &[(&str, &str)],
    ) -> InstallKind {
        let files: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(p, c)| (PathBuf::from(p), c.to_string()))
            .collect();
        let exists = |p: &Path| files.iter().any(|(f, _)| f == p);
        let read = |p: &Path| files.iter().find(|(f, _)| f == p).map(|(_, c)| c.clone());
        InstallKind::detect(&Environment {
            os,
            app_id: "cratetune",
            exe: Path::new(exe),
            appimage: None,
            packaged,
            sandbox,
            exists: &exists,
            read: &read,
        })
    }

    fn managed(manager: PackageManager) -> InstallKind {
        InstallKind::Manual(ManualReason::PackageManager(manager))
    }

    #[test]
    fn parses_package_manager_names() {
        assert_eq!(
            PackageManager::parse("flatpak"),
            Some(PackageManager::Flatpak)
        );
        assert_eq!(PackageManager::parse(" AUR\n"), Some(PackageManager::Aur));
        assert_eq!(
            PackageManager::parse("brew"),
            Some(PackageManager::Homebrew)
        );
        assert_eq!(PackageManager::parse("Scoop"), Some(PackageManager::Scoop));
        assert_eq!(PackageManager::parse("snap"), Some(PackageManager::Snap));
        assert_eq!(
            PackageManager::parse("Nix\nignored"),
            Some(PackageManager::Other("Nix".into()))
        );
        for off in ["", "  ", "0", "no", "false", "None"] {
            assert_eq!(PackageManager::parse(off), None, "{off:?}");
        }
        assert_eq!(
            PackageManager::parse(&"x".repeat(100)).unwrap().id().len(),
            40
        );
        assert_eq!(PackageManager::env_var("cratetune"), "CRATETUNE_PACKAGED");
        assert_eq!(PackageManager::env_var("my-app.x"), "MY_APP_X_PACKAGED");
        assert_eq!(PackageManager::Aur.display_name(), "AUR");
    }

    #[test]
    fn package_managers_turn_self_update_off() {
        // The environment variable wins over everything, on every OS.
        let kind = detect_packaged(
            Os::Linux,
            "/home/me/.local/bin/cratetune",
            Some("aur"),
            None,
            &[("/home/me/.local/share/applications/cratetune.desktop", "")],
        );
        assert_eq!(kind, managed(PackageManager::Aur));
        assert!(!kind.can_self_update());
        assert_eq!(kind.package_manager(), Some(&PackageManager::Aur));
        assert_eq!(kind.name(), "package-manager");
        assert_eq!(kind.asset_suffix(Arch::X86_64), None);
        assert!(kind.writable_dirs().is_empty());
        assert_eq!(
            detect_packaged(
                Os::Windows,
                "C:/Tools/CrateTune/cratetune.exe",
                Some("scoop"),
                None,
                &[]
            ),
            managed(PackageManager::Scoop)
        );
        // An "off" value falls through to the normal detection.
        assert_eq!(
            detect_packaged(
                Os::Linux,
                "/home/me/.local/bin/cratetune",
                Some("0"),
                None,
                &[("/home/me/.local/share/applications/cratetune.desktop", "")],
            ),
            InstallKind::LinuxTarball {
                prefix: "/home/me/.local".into(),
                exe: "/home/me/.local/bin/cratetune".into()
            }
        );
        // Flatpak and Snap sandboxes.
        assert_eq!(
            detect_packaged(
                Os::Linux,
                "/app/bin/cratetune",
                None,
                Some(PackageManager::Flatpak),
                &[]
            ),
            managed(PackageManager::Flatpak)
        );
        assert_eq!(
            detect_packaged(
                Os::Linux,
                "/snap/cratetune/12/bin/cratetune",
                None,
                Some(PackageManager::Snap),
                &[]
            ),
            managed(PackageManager::Snap)
        );
        assert_eq!(
            detect_packaged(
                Os::Linux,
                "/app/bin/cratetune",
                Some("flatpak"),
                Some(PackageManager::Snap),
                &[]
            ),
            managed(PackageManager::Flatpak)
        );
    }

    #[test]
    fn package_managers_from_marker_files() {
        // AUR: /usr/share/cratetune/packaged.
        assert_eq!(
            detect_packaged(
                Os::Linux,
                "/usr/bin/cratetune",
                None,
                None,
                &[("/usr/share/cratetune/packaged", "aur\n")]
            ),
            managed(PackageManager::Aur)
        );
        // Without the marker, a /usr install is still a nameless system package.
        assert_eq!(
            detect_packaged(Os::Linux, "/usr/bin/cratetune", None, None, &[]),
            InstallKind::Manual(ManualReason::SystemPackage)
        );
        // An empty marker is ignored.
        assert_eq!(
            detect_packaged(
                Os::Linux,
                "/usr/bin/cratetune",
                None,
                None,
                &[("/usr/share/cratetune/packaged", "")]
            ),
            InstallKind::Manual(ManualReason::SystemPackage)
        );
        // Windows: next to the exe (Scoop's post_install writes it).
        assert_eq!(
            detect_packaged(
                Os::Windows,
                "D:/Apps/CrateTune/cratetune.exe",
                None,
                None,
                &[("D:/Apps/CrateTune/packaged", "scoop")]
            ),
            managed(PackageManager::Scoop)
        );
        // macOS: inside the bundle's Resources.
        assert_eq!(
            detect_packaged(
                Os::MacOs,
                "/Applications/CrateTune.app/Contents/MacOS/music-gpui",
                None,
                None,
                &[(
                    "/Applications/CrateTune.app/Contents/Resources/packaged",
                    "homebrew"
                )]
            ),
            managed(PackageManager::Homebrew)
        );
    }

    #[test]
    fn package_managers_from_install_paths() {
        // Homebrew cask: a Caskroom entry for the app id, Apple silicon or Intel.
        for room in [
            "/opt/homebrew/Caskroom/cratetune",
            "/usr/local/Caskroom/cratetune",
        ] {
            assert_eq!(
                detect_packaged(
                    Os::MacOs,
                    "/Applications/CrateTune.app/Contents/MacOS/music-gpui",
                    None,
                    None,
                    &[(room, "")]
                ),
                managed(PackageManager::Homebrew)
            );
        }
        // Another cask, or a build from source next to the cask, is not managed.
        assert_eq!(
            detect_packaged(
                Os::MacOs,
                "/Applications/CrateTune.app/Contents/MacOS/music-gpui",
                None,
                None,
                &[("/opt/homebrew/Caskroom/other", "")]
            ),
            InstallKind::MacBundle {
                bundle: "/Applications/CrateTune.app".into()
            }
        );
        assert_eq!(
            detect_packaged(
                Os::MacOs,
                "/Users/me/src/target/release/music-gpui",
                None,
                None,
                &[("/opt/homebrew/Caskroom/cratetune", "")]
            ),
            InstallKind::Manual(ManualReason::DevBuild)
        );
        // Scoop: <root>/apps/<id>/<version>/<id>.exe (`current` resolves to the version).
        assert_eq!(
            detect_packaged(
                Os::Windows,
                "C:/Users/me/scoop/apps/cratetune/0.2.6/cratetune.exe",
                None,
                None,
                &[]
            ),
            managed(PackageManager::Scoop)
        );
        assert_eq!(
            detect_packaged(
                Os::Windows,
                "C:/Users/me/Scoop/Apps/CrateTune/current/CrateTune.exe",
                None,
                None,
                &[]
            ),
            managed(PackageManager::Scoop)
        );
        assert_eq!(
            detect_packaged(
                Os::Windows,
                "C:/Users/me/scoop/apps/other/1.0/cratetune.exe",
                None,
                None,
                &[]
            ),
            InstallKind::WindowsPortable {
                exe: "C:/Users/me/scoop/apps/other/1.0/cratetune.exe".into()
            }
        );
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
