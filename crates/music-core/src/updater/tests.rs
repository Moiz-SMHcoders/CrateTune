//! Check → download → verify against a local stand-in for the GitHub API.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use super::*;
use crate::testutil::{Reply, TestServer};

/// Path prefix → (status, body); edited by tests.
type Routes = Arc<Mutex<Vec<(String, u16, Vec<u8>)>>>;

struct Fixture {
    _server: TestServer,
    base: String,
    dir: PathBuf,
    asset_name: String,
    routes: Routes,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn kind_in(dir: &Path) -> InstallKind {
    InstallKind::AppImage {
        path: dir.join("app/TuneDesk.AppImage"),
    }
}

fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("updater-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("app")).unwrap();
    let suffix = kind_in(&dir)
        .asset_suffix(Arch::current())
        .unwrap_or("-x86_64.AppImage");
    let asset_name = format!("tunedesk-0.2.0{suffix}");
    let routes: Routes = Arc::new(Mutex::new(Vec::new()));
    let served = routes.clone();
    let server = TestServer::start(move |path, _, _| {
        let routes = served.lock().unwrap();
        match routes.iter().find(|(p, _, _)| path.starts_with(p.as_str())) {
            Some((_, status, body)) => Reply::Full {
                status: *status,
                headers: vec!["Content-Type: application/octet-stream".into()],
                body: body.clone(),
            },
            None => Reply::Status(404),
        }
    });
    let base = server.base.clone();
    let package = b"new version bytes".repeat(1000);
    let hash = sums::hex(&Sha256::digest(&package));
    let release = |tag: &str, prerelease: bool, with_sums: bool| {
        let mut assets = vec![serde_json::json!({
            "name": asset_name,
            "browser_download_url": format!("{base}/dl/{tag}/{asset_name}"),
            "size": package.len(),
        })];
        if with_sums {
            assets.push(serde_json::json!({
                "name": "SHA256SUMS.txt",
                "browser_download_url": format!("{base}/dl/{tag}/SHA256SUMS.txt"),
                "size": 100,
            }));
        }
        serde_json::json!({
            "tag_name": tag, "prerelease": prerelease, "draft": false,
            "html_url": format!("{base}/page/{tag}"), "assets": assets,
        })
    };
    let list = serde_json::json!([
        release("v0.2.0", false, true),
        release("v0.3.0-beta.1", true, false)
    ]);
    {
        let mut r = routes.lock().unwrap();
        r.push((
            "/repos/o/r/releases".into(),
            200,
            list.to_string().into_bytes(),
        ));
        r.push((
            "/dl/v0.2.0/SHA256SUMS.txt".into(),
            200,
            format!("{hash}  {asset_name}\n").into_bytes(),
        ));
        r.push((format!("/dl/v0.2.0/{asset_name}"), 200, package));
    }
    Fixture {
        _server: server,
        base,
        dir,
        asset_name,
        routes,
    }
}

fn updater(f: &Fixture) -> Updater {
    Updater::new(
        Source::with_api_base("o/r", &f.base),
        "tunedesk",
        Version::new(0, 1, 0),
        f.dir.join("cache/updates"),
        kind_in(&f.dir),
    )
}

#[test]
fn checks_downloads_and_verifies() {
    let f = fixture("ok");
    let u = updater(&f);
    assert_eq!(u.blocker(), None);
    // Stable only: 0.2.0 (the beta needs pre-releases on).
    let offer = u.check(false).unwrap().unwrap();
    assert_eq!(offer.version, Version::new(0, 2, 0));
    assert_eq!(offer.asset.as_ref().unwrap().name, f.asset_name);
    assert_eq!(
        u.check(true).unwrap().unwrap().version.to_string(),
        "0.3.0-beta.1"
    );
    let mut seen = Vec::new();
    let staged = u
        .download(&offer, &AtomicBool::new(false), &mut |done, total| {
            seen.push((done, total))
        })
        .unwrap();
    assert_eq!(seen.last(), Some(&(17_000, Some(17_000))));
    assert_eq!(sha256_file(&staged.file).unwrap(), staged.sha256);
    assert!(staged.file.starts_with(f.dir.join("cache/updates/0.2.0")));
    // A second download reuses the verified file.
    let again = u
        .download(&offer, &AtomicBool::new(false), &mut |_, _| {})
        .unwrap();
    assert_eq!(again, staged);
    // Up to date once running 0.2.0; cleanup removes its download.
    let mut current = u.clone();
    current.current = Version::new(0, 2, 0);
    assert_eq!(current.check(false).unwrap(), None);
    current.cleanup();
    assert!(!f.dir.join("cache/updates/0.2.0").exists());
    let log = std::fs::read_to_string(f.dir.join("cache/updates/update.log")).unwrap();
    assert!(log.contains("matches SHA256SUMS.txt"), "{log}");
}

#[test]
fn refuses_a_bad_or_missing_checksum() {
    let f = fixture("bad");
    let u = updater(&f);
    let offer = u.check(false).unwrap().unwrap();
    {
        let mut routes = f.routes.lock().unwrap();
        let sums = routes
            .iter_mut()
            .find(|(p, _, _)| p.ends_with("SHA256SUMS.txt"))
            .unwrap();
        sums.2 = format!("{}  {}\n", "ab".repeat(32), f.asset_name).into_bytes();
    }
    let err = u
        .download(&offer, &AtomicBool::new(false), &mut |_, _| {})
        .unwrap_err();
    assert!(matches!(err, UpdateError::ChecksumMismatch { .. }), "{err}");
    // Nothing is left to install.
    let dir = f.dir.join("cache/updates/0.2.0");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

    // Not listed.
    f.routes.lock().unwrap().iter_mut().for_each(|r| {
        if r.0.ends_with("SHA256SUMS.txt") {
            r.2 = format!("{}  other.zip\n", "ab".repeat(32)).into_bytes();
        }
    });
    assert!(matches!(
        u.download(&offer, &AtomicBool::new(false), &mut |_, _| {}),
        Err(UpdateError::NotListed(_))
    ));

    // No sums file in the release at all.
    let mut no_sums = offer.clone();
    no_sums.sums = None;
    assert_eq!(
        u.download(&no_sums, &AtomicBool::new(false), &mut |_, _| {}),
        Err(UpdateError::NoChecksums)
    );

    // A sums URL that went missing (404).
    f.routes
        .lock()
        .unwrap()
        .retain(|r| !r.0.ends_with("SHA256SUMS.txt"));
    assert!(matches!(
        u.download(&offer, &AtomicBool::new(false), &mut |_, _| {}),
        Err(UpdateError::Network(_))
    ));
}

#[test]
fn install_refuses_a_tampered_download_and_unwritable_installs() {
    let f = fixture("tamper");
    let u = updater(&f);
    let offer = u.check(false).unwrap().unwrap();
    let staged = u
        .download(&offer, &AtomicBool::new(false), &mut |_, _| {})
        .unwrap();
    std::fs::write(&staged.file, b"swapped after verification").unwrap();
    assert!(matches!(
        u.install(&staged, false),
        Err(UpdateError::ChecksumMismatch { .. })
    ));
    let installed = f.dir.join("app/TuneDesk.AppImage");
    assert!(!installed.exists());

    // A folder that can't be written.
    let mut elsewhere = u.clone();
    elsewhere.kind = InstallKind::AppImage {
        path: f.dir.join("missing-dir/TuneDesk.AppImage"),
    };
    assert!(matches!(
        elsewhere.blocker(),
        Some(UpdateError::NotWritable(_))
    ));
    assert!(matches!(
        elsewhere.download(&offer, &AtomicBool::new(false), &mut |_, _| {}),
        Err(UpdateError::NotWritable(_))
    ));
    let mut manual = u.clone();
    manual.kind = InstallKind::Manual(ManualReason::DevBuild);
    assert_eq!(
        manual.blocker(),
        Some(UpdateError::Manual(ManualReason::DevBuild))
    );
}

#[test]
fn cancels_and_rejects_foreign_urls() {
    let f = fixture("cancel");
    let u = updater(&f);
    let offer = u.check(false).unwrap().unwrap();
    let cancel = AtomicBool::new(false);
    let err = u
        .download(&offer, &cancel, &mut |done, _| {
            if done > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
    // Checked before every read, so even a body that arrived in one read is cancelled.
    assert!(err == UpdateError::Cancelled, "{err}");
    assert!(
        !f.dir
            .join("cache/updates/0.2.0")
            .join(&f.asset_name)
            .exists()
    );
    let part = f
        .dir
        .join("cache/updates/0.2.0")
        .join(format!("{}.part", f.asset_name));
    assert!(!part.exists());

    let mut foreign = offer.clone();
    foreign.asset.as_mut().unwrap().url = "https://example.com/x.AppImage".into();
    assert!(matches!(
        u.download(&foreign, &AtomicBool::new(false), &mut |_, _| {}),
        Err(UpdateError::UntrustedUrl(_))
    ));
}

#[cfg(unix)]
#[test]
fn installs_an_appimage_end_to_end() {
    let f = fixture("install");
    let u = updater(&f);
    let installed = f.dir.join("app/TuneDesk.AppImage");
    std::fs::write(&installed, b"old").unwrap();
    let offer = u.check(false).unwrap().unwrap();
    let staged = u
        .download(&offer, &AtomicBool::new(false), &mut |_, _| {})
        .unwrap();
    u.install(&staged, false).unwrap();
    assert_eq!(
        std::fs::read(&installed).unwrap(),
        b"new version bytes".repeat(1000)
    );
}

#[test]
fn default_dirs() {
    assert_eq!(
        default_dir(Some(Path::new("/c")), Some(Path::new("/cfg"))),
        Some(PathBuf::from("/cfg/cache/updates"))
    );
    assert_eq!(
        default_dir(Some(Path::new("/c")), None),
        Some(PathBuf::from("/c/updates"))
    );
}
