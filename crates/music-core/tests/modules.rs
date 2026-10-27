//! Provider modules end to end: package validation, the JS sandbox, search →
//! match → stream → decode through the player, resolver chaining, URL /
//! descriptor / catalog installs and updates, settings and secrets, downloads.

mod common;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{Req, Resp, Server, file_response, fixture, temp_dir, wav};
use music_core::modules::{
    CheckStatus, Failure, FileSecrets, Import, ModuleResolver, Modules, package,
};
use music_core::{
    Error, PlaybackStatus, Player, Quality, ResolvedStream, SourceKind, StreamResolver, Track,
};
use serde_json::{Value, json};

const APP: &str = "1.0.0";

fn echo_dir() -> PathBuf {
    fixture("modules/echo")
}

fn open(dir: &Path) -> Modules {
    Modules::open(
        dir.join("modules"),
        Arc::new(FileSecrets::new(dir.join("secrets.json"))),
        APP,
    )
    .unwrap()
}

/// Packs a module directory written from `manifest` (the `module` object is
/// completed with defaults) and `script`.
fn pack(name: &str, module: Value, script: &str) -> Vec<u8> {
    let dir = temp_dir(name);
    let mut base = json!({
        "id": format!("test.{name}"), "name": name, "version": "1.0.0",
        "capabilities": ["searchTracks", "getTrackStreamUrl"],
    });
    if let (Some(b), Some(m)) = (base.as_object_mut(), module.as_object()) {
        for (k, v) in m {
            b.insert(k.clone(), v.clone());
        }
    }
    let manifest = json!({"format": "archivetune.module", "formatVersion": 1, "apiVersion": 1,
        "entry": "provider.js", "module": base});
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(dir.join("provider.js"), script).unwrap();
    package::pack_dir(&dir).unwrap()
}

fn install(modules: &Modules, bytes: &[u8]) -> String {
    match modules.import_bytes(bytes, "").unwrap() {
        Import::Module(m) => modules.commit(Import::Module(m)).unwrap(),
        Import::Catalog(_) => panic!("not a module"),
    }
}

/// A raw ZIP with the given entries (names and bytes as is).
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut out);
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
            } else {
                w.start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap();
    }
    out.into_inner()
}

fn sha(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn manifest_with(script: &[u8], edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut m = json!({"format": "archivetune.module", "formatVersion": 1, "apiVersion": 1,
        "entry": "provider.js", "sha256": sha(script),
        "module": {"id": "test.bad", "name": "Bad", "version": "1",
                   "capabilities": ["searchTracks", "getTrackStreamUrl"]}});
    edit(&mut m);
    m.to_string().into_bytes()
}

const GOOD_SCRIPT: &[u8] =
    b"module.exports = { searchTracks: async () => [], getTrackStreamUrl: async () => ({}) };";

fn code(err: music_core::modules::ModuleError) -> String {
    err.code.unwrap_or_default()
}

#[test]
fn committed_echo_fixture_matches_its_source() {
    let packed = package::pack_dir(&echo_dir()).unwrap();
    let committed = std::fs::read(fixture("modules/echo.arch")).unwrap_or_default();
    if packed != committed {
        // Regenerate: cargo run -p music-core --example pack_module -- \
        //   crates/music-core/tests/fixtures/modules/echo crates/music-core/tests/fixtures/modules/echo.arch
        panic!("tests/fixtures/modules/echo.arch is stale; run the pack_module example");
    }
    let pkg = package::read(&committed, APP).unwrap();
    assert_eq!(pkg.manifest.id, "dev.music-macos.echo");
    assert_eq!(pkg.manifest.title(), "Echo (test provider)");
    assert!(pkg.manifest.icon.is_some());
    let quality = pkg.manifest.setting("quality").unwrap();
    assert_eq!(quality.label, "Audio quality");
    assert_eq!(quality.options.len(), 2);
    assert!(pkg.manifest.setting("apiToken").unwrap().secret);
}

#[test]
fn install_validation_rejects_bad_packages() {
    let dir = temp_dir("modules-validation");
    let modules = open(&dir);
    let err = |bytes: &[u8]| modules.import_bytes(bytes, "").unwrap_err();
    let good = manifest_with(GOOD_SCRIPT, |_| {});
    // The baseline is accepted.
    assert!(matches!(
        modules.import_bytes(
            &zip(&[("manifest.json", &good), ("provider.js", GOOD_SCRIPT)]),
            ""
        ),
        Ok(Import::Module(_))
    ));
    // Not a module document at all.
    assert_eq!(code(err(b"console.log('hi')")), "UNSUPPORTED_DOCUMENT");
    assert_eq!(code(err(b"PK\x03\x04garbage")), "ARCH_ZIP");
    // Entries.
    let extra = zip(&[
        ("manifest.json", &good),
        ("provider.js", GOOD_SCRIPT),
        ("notes.txt", b"x"),
    ]);
    assert_eq!(code(err(&extra)), "ARCH_ENTRY");
    let nested = zip(&[("manifest.json", &good), ("src/provider.js", GOOD_SCRIPT)]);
    assert_eq!(code(err(&nested)), "ARCH_ENTRY");
    let directory = zip(&[
        ("manifest.json", &good),
        ("provider.js", GOOD_SCRIPT),
        ("icon.png/", b""),
    ]);
    assert_eq!(code(err(&directory)), "ARCH_ENTRY");
    assert_eq!(
        code(err(&zip(&[("manifest.json", &good)]))),
        "ARCH_SCRIPT_MISSING"
    );
    assert_eq!(
        code(err(&zip(&[("provider.js", GOOD_SCRIPT)]))),
        "ARCH_MANIFEST_MISSING"
    );
    // Duplicate entries: write "manifesX.json" and rename it in place.
    let mut dup = zip(&[
        ("manifest.json", &good),
        ("manifesX.json", &good),
        ("provider.js", GOOD_SCRIPT),
    ]);
    for i in 0..dup.len() - 13 {
        if &dup[i..i + 13] == b"manifesX.json" {
            dup[i + 7] = b't';
        }
    }
    assert!(matches!(
        code(err(&dup)).as_str(),
        "ARCH_ENTRY" | "ARCH_ZIP"
    ));
    // Size.
    let big = vec![b' '; 4 * 1024 * 1024 + 1];
    assert_eq!(
        code(err(&[b"PK\x03\x04".as_slice(), &big].concat())),
        "ARCH_SIZE"
    );
    // Checksum, versions, identity, capabilities, settings, features, icon.
    type Edit = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Edit)> = vec![
        (
            "ARCH_CHECKSUM",
            Box::new(|m| m["sha256"] = json!("0".repeat(64))),
        ),
        ("ARCH_CHECKSUM", Box::new(|m| m["sha256"] = json!("nope"))),
        ("ARCH_VERSION", Box::new(|m| m["formatVersion"] = json!(2))),
        ("ARCH_VERSION", Box::new(|m| m["apiVersion"] = json!(1.0))),
        (
            "ARCH_VERSION",
            Box::new(|m| m["format"] = json!("other.module")),
        ),
        ("ARCH_ENTRY", Box::new(|m| m["entry"] = json!("index.js"))),
        (
            "ARCH_IDENTITY",
            Box::new(|m| m["module"]["id"] = json!("Bad ID")),
        ),
        (
            "ARCH_IDENTITY",
            Box::new(|m| m["module"]["version"] = json!(1)),
        ),
        (
            "ARCH_CAPABILITIES",
            Box::new(|m| {
                m["module"]["capabilities"] =
                    json!(["searchTracks", "getTrackStreamUrl", "searchTracks"])
            }),
        ),
        (
            "ARCH_CAPABILITIES",
            Box::new(|m| {
                m["module"]["capabilities"] =
                    json!(["searchTracks", "getTrackStreamUrl", "teleport"])
            }),
        ),
        (
            "ARCH_SETTING_TYPE",
            Box::new(|m| m["module"]["settings"] = json!({"x": {"type": "color"}})),
        ),
        (
            "ARCH_SETTINGS",
            Box::new(|m| {
                m["module"]["settings"] = json!({"n": {"type": "number", "min": 5, "max": 1}})
            }),
        ),
        (
            "ARCH_SETTINGS",
            Box::new(|m| m["module"]["settings"] = json!({"s": {"type": "select", "options": []}})),
        ),
        (
            "ARCH_SETTINGS",
            Box::new(|m| {
                m["module"]["settings"] = json!({"b": {"type": "boolean", "defaultValue": "yes"}})
            }),
        ),
        (
            "ARCH_QUALITY_OPTIONS",
            Box::new(|m| {
                m["module"]["qualityOptions"] =
                    json!([{"id": "a", "label": "A"}, {"id": "a", "label": "B"}])
            }),
        ),
        (
            "ARCH_QUALITY_OPTIONS",
            Box::new(|m| m["module"]["defaultQuality"] = json!("x")),
        ),
        (
            "ARCH_RUNTIME_FEATURE",
            Box::new(|m| m["module"]["requiredRuntimeFeatures"] = json!(["teleport@1"])),
        ),
        (
            "ARCH_APP_VERSION",
            Box::new(|m| m["module"]["minAppVersion"] = json!("99.0.0")),
        ),
        (
            "ARCH_UNSUPPORTED_permissions",
            Box::new(|m| m["module"]["permissions"] = json!({})),
        ),
        (
            "ARCH_UNSUPPORTED_runtime",
            Box::new(|m| m["runtime"] = json!("extension")),
        ),
        (
            "ARCH_ICON",
            Box::new(|m| m["module"]["icon"] = json!("icon.png")),
        ),
        (
            "ARCH_LABELS",
            Box::new(|m| m["module"]["labels"] = json!([1])),
        ),
    ];
    for (expected, edit) in cases {
        let manifest = manifest_with(GOOD_SCRIPT, |m| edit(m));
        let bytes = zip(&[("manifest.json", &manifest), ("provider.js", GOOD_SCRIPT)]);
        assert_eq!(
            code(err(&bytes)),
            expected,
            "{}",
            String::from_utf8_lossy(&manifest)
        );
    }
    // Deeply nested manifests.
    let mut deep = String::from("1");
    for _ in 0..40 {
        deep = format!("[{deep}]");
    }
    let manifest = manifest_with(GOOD_SCRIPT, |m| {
        m["module"]["extra"] = serde_json::from_str(&deep).unwrap()
    });
    assert_eq!(
        code(err(&zip(&[
            ("manifest.json", &manifest),
            ("provider.js", GOOD_SCRIPT)
        ]))),
        "ARCH_MANIFEST_DEPTH"
    );
    // A bad PNG behind a valid checksum.
    let png = b"not a png at all, not at all";
    let manifest = manifest_with(GOOD_SCRIPT, |m| {
        m["module"]["icon"] = json!("icon.png");
        m["iconSha256"] = json!(sha(png));
    });
    assert_eq!(
        code(err(&zip(&[
            ("manifest.json", &manifest),
            ("provider.js", GOOD_SCRIPT),
            ("icon.png", png)
        ]))),
        "ARCH_ICON"
    );
    // Not UTF-8.
    let latin1 = b"module.exports = {}; // \xe9";
    let manifest = manifest_with(latin1, |_| {});
    assert_eq!(
        code(err(&zip(&[
            ("manifest.json", &manifest),
            ("provider.js", latin1)
        ]))),
        "ARCH_UTF8"
    );
    // Exports must match the declared capabilities (checked by running the script).
    let partial = pack(
        "partial",
        json!({}),
        "module.exports = { searchTracks() { return []; } };",
    );
    let e = err(&partial);
    assert_eq!(e.failure, Failure::Execution);
    assert!(
        code(e).starts_with("TypeError"),
        "capability mismatch is a TypeError"
    );
    let extra = pack(
        "extra",
        json!({}),
        "module.exports = { searchTracks(){}, getTrackStreamUrl(){}, getAlbum(){} };",
    );
    assert!(code(err(&extra)).starts_with("TypeError"));
    let not_object = pack("notobj", json!({}), "module.exports = 42;");
    assert!(code(err(&not_object)).starts_with("TypeError"));
    let syntax = pack("syntax", json!({}), "module.exports = {");
    assert!(code(err(&syntax)).starts_with("SyntaxError"));
    // Nothing was installed by any of this.
    assert!(modules.registry().modules.is_empty());
}

/// A module whose methods run `body` (with `query` / `context` in scope).
fn probe(name: &str, search_body: &str) -> Vec<u8> {
    pack(
        name,
        json!({}),
        &format!(
            "async function searchTracks(query, limit, context) {{ {search_body} }}\n\
             async function getTrackStreamUrl(id) {{ return {{url: 'https://example.invalid/' + id}}; }}\n\
             module.exports = {{ searchTracks, getTrackStreamUrl }};"
        ),
    )
}

#[test]
fn sandbox_limits_and_host_apis() {
    let dir = temp_dir("modules-sandbox");
    let modules = open(&dir);
    modules.set_timeouts(Duration::from_millis(1500), Duration::from_secs(5));

    // Forbidden / absent APIs.
    let id = install(
        &modules,
        &probe(
            "globals",
            "const kinds = ['require','process','std','os','Deno','Bun','window','document','XMLHttpRequest','WebSocket','importScripts','__moduleFetchStart','__moduleStorage']
               .map(name => name + ':' + typeof globalThis[name]);
             let imported = 'none';
             try { await import('fs'); imported = 'loaded'; } catch (e) { imported = 'rejected'; }
             let file = 'none';
             try { await fetch('file:///etc/passwd'); file = 'fetched'; } catch (e) { file = e.name; }
             return [{id: 'x', title: kinds.join(',') + ';import=' + imported + ';file=' + file, artists: ['a']}];",
        ),
    );
    let tracks = modules.search(&id, "q").unwrap();
    let title = &tracks[0].title;
    for name in [
        "require",
        "process",
        "std",
        "os",
        "Deno",
        "window",
        "document",
        "XMLHttpRequest",
        "__moduleFetchStart",
        "__moduleStorage",
    ] {
        assert!(
            title.contains(&format!("{name}:undefined")),
            "{name} must not exist: {title}"
        );
    }
    assert!(title.contains("import=rejected"), "{title}");
    assert!(title.contains("file=TypeError"), "{title}");

    // A busy loop is interrupted at the deadline, and the engine recovers.
    let id = install(
        &modules,
        &probe(
            "busy",
            "if (query === 'spin') { while (true) {} } return [];",
        ),
    );
    let started = Instant::now();
    let e = modules.search(&id, "spin").unwrap_err();
    assert_eq!(e.failure, Failure::Timeout, "{e:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "interrupted promptly"
    );
    assert!(
        modules.search(&id, "ok").unwrap().is_empty(),
        "a fresh engine works again"
    );

    // Memory is capped at 32 MiB.
    let id = install(
        &modules,
        &probe(
            "memory",
            "const hog = []; while (true) hog.push(new Array(100000).fill(1)); return [];",
        ),
    );
    let e = modules.search(&id, "q").unwrap_err();
    assert!(
        matches!(e.failure, Failure::Execution | Failure::Timeout),
        "{e:?}"
    );

    // Deep recursion hits the 1 MiB stack limit as a RangeError / InternalError.
    let id = install(
        &modules,
        &probe("stack", "function f() { return f() + 1; } return f();"),
    );
    let e = modules.search(&id, "q").unwrap_err();
    assert_eq!(e.failure, Failure::Execution, "{e:?}");

    // A promise that never settles (and has nothing pending) fails fast.
    let id = install(
        &modules,
        &probe("unsettled", "return new Promise(() => {});"),
    );
    let started = Instant::now();
    let e = modules.search(&id, "q").unwrap_err();
    assert_eq!(
        e.code.as_deref(),
        Some("PROVIDER/UNSETTLED").map(|_| "UNSETTLED"),
        "{e:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(1));

    // Timers, clearTimeout, the 128-timer cap, console → log, storage, utils, crypto, URL.
    let id = install(
        &modules,
        &probe(
            "apis",
            "const order = [];
             await new Promise(resolve => setTimeout(() => { order.push('a'); resolve(); }, 30));
             const cancelled = setTimeout(() => order.push('never'), 10);
             clearTimeout(cancelled);
             await new Promise(resolve => setTimeout(resolve, 40));
             let capped = 'no';
             try { for (let i = 0; i < 200; i++) setTimeout(() => {}, 10000); } catch (e) { capped = e.name; }
             console.log('hello from', 'the module', {n: 1});
             const visits = (storage.get('visits') || 0) + 1;
             storage.set('visits', visits);
             const hash = utils.sha256('abc');
             const bytes = crypto.getRandomValues(new Uint8Array(8));
             const url = new URL('../b?x=1', 'https://example.org/a/c');
             url.searchParams.set('y', '2');
             const text = new TextDecoder().decode(new TextEncoder().encode('ü'));
             return [{id: 'x', artists: ['a'], title: [order.join(''), capped, visits, hash.slice(0, 8),
               bytes.length, url.href, text, btoa('hi'), atob('aGk=')].join('|')}];",
        ),
    );
    let first = modules.search(&id, "q").unwrap()[0].title.clone();
    let parts: Vec<&str> = first.split('|').collect();
    assert_eq!(parts[0], "a", "only the uncancelled timer fired: {first}");
    assert_eq!(parts[1], "TypeError", "more than 128 timers are refused");
    assert_eq!(parts[2], "1");
    assert_eq!(parts[3], "ba7816bf");
    assert_eq!(parts[4], "8");
    assert_eq!(parts[5], "https://example.org/b?x=1&y=2");
    assert_eq!(parts[6], "ü");
    assert_eq!(&parts[7..], ["aGk=", "hi"]);
    let second = modules.search(&id, "q").unwrap()[0].title.clone();
    assert_eq!(
        second.split('|').nth(2),
        Some("2"),
        "storage persists across calls"
    );
    let log = modules.health(&id).log;
    assert!(
        log.iter()
            .any(|l| l.text.contains("hello from the module {\"n\":1}")),
        "{log:?}"
    );

    // Thrown errors are classified with a redacted code.
    let id = install(
        &modules,
        &probe("throws", "throw new RangeError('secret token abc');"),
    );
    let e = modules.search(&id, "q").unwrap_err();
    assert_eq!(e.failure, Failure::Execution);
    let code = e.code.unwrap();
    assert!(code.starts_with("RangeError"), "{code}");
    assert!(
        !code.contains("secret"),
        "messages are not part of the code"
    );

    // Fetch: HTTP errors don't throw, transport errors do, limits apply.
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let server = Server::start(move |req: &Req| {
        counter.fetch_add(1, Ordering::SeqCst);
        match req.path.as_str() {
            "/missing" => Resp::status(404),
            "/big" => Resp::ok("application/octet-stream", vec![0u8; 9 * 1024 * 1024]),
            "/slow" => {
                std::thread::sleep(Duration::from_millis(3000));
                Resp::ok("text/plain", "late")
            }
            "/redirect" => Resp::redirect("/echo"),
            _ => Resp::json(
                &json!({"method": req.method, "auth": req.headers.get("authorization"),
                                    "body": String::from_utf8_lossy(&req.body)}),
            ),
        }
    });
    let id = install(
        &modules,
        &probe(
            "fetching",
            &format!(
                "const base = '{base}';
                 if (query === 'status') {{ const r = await fetch(base + '/missing'); return [{{id: 'x', artists: ['a'], title: r.status + ':' + r.ok}}]; }}
                 if (query === 'post') {{ const r = await fetch(base + '/echo', {{method: 'POST', body: JSON.stringify({{a: 1}}), headers: {{Authorization: 'Bearer t'}}}}); const j = await r.json(); return [{{id: 'x', artists: ['a'], title: j.method + ':' + j.auth + ':' + j.body}}]; }}
                 if (query === 'redirect') {{ const r = await fetch(base + '/redirect', {{headers: {{Authorization: 'Bearer t'}}}}); const j = await r.json(); return [{{id: 'x', artists: ['a'], title: r.url + ':' + j.auth}}]; }}
                 if (query === 'parallel') {{ const rs = await Promise.all([1,2,3,4,5,6,7,8,9,10].map(i => fetch(base + '/echo?i=' + i))); return [{{id: 'x', artists: ['a'], title: String(rs.filter(r => r.ok).length)}}]; }}
                 if (query === 'big') {{ await fetch(base + '/big'); return []; }}
                 if (query === 'abort') {{ const c = new AbortController(); setTimeout(() => c.abort(), 50); try {{ await fetch(base + '/slow', {{signal: c.signal}}); }} catch (e) {{ return [{{id: 'x', artists: ['a'], title: e.name}}]; }} return []; }}
                 if (query === 'flood') {{ const all = []; for (let i = 0; i < 70; i++) all.push(fetch(base + '/echo').catch(e => e.name)); const r = await Promise.all(all); return [{{id: 'x', artists: ['a'], title: String(r.filter(x => x === 'TypeError').length)}}]; }}
                 return [];",
                base = server.base
            ),
        ),
    );
    let title = |q: &str| modules.search(&id, q).unwrap()[0].title.clone();
    assert_eq!(title("status"), "404:false");
    assert_eq!(title("post"), "POST:Bearer t:{\"a\":1}");
    assert_eq!(
        title("redirect"),
        format!("{}/echo:Bearer t", server.base),
        "same-origin redirect keeps auth"
    );
    assert_eq!(title("parallel"), "10");
    assert_eq!(title("abort"), "AbortError");
    let flood = title("flood");
    assert_eq!(flood, "6", "requests past 64 per call are refused");
    let e = modules.search(&id, "big").unwrap_err();
    assert_eq!(e.failure, Failure::InvalidDocument, "{e:?}");
    assert!(hits.load(Ordering::SeqCst) > 0);
}

/// An echo server: `catalog.json` + `audio/<file>`. `ranges` = honour Range requests.
fn echo_server(
    entries: Value,
    files: Vec<(&'static str, Vec<u8>, &'static str)>,
    ranges: bool,
) -> Server {
    Server::start(move |req: &Req| {
        if req.path == "/catalog.json" {
            return Resp::json(&entries);
        }
        if let Some(name) = req.path.strip_prefix("/audio/")
            && let Some((_, data, mime)) = files.iter().find(|(n, _, _)| *n == name)
        {
            return if ranges {
                file_response(req, data, mime)
            } else {
                Resp::ok(mime, data.clone())
            };
        }
        Resp::status(404)
    })
}

fn catalog_entry(id: &str, title: &str, artist: &str, secs: f64, variants: Value) -> Value {
    json!({"id": id, "title": title, "artists": [artist], "album": "Album", "durationMs": secs * 1000.0, "variants": variants})
}

fn install_echo(modules: &Modules, base: &str, token: Option<&str>) -> String {
    let bytes = std::fs::read(fixture("modules/echo.arch")).unwrap();
    let id = install(modules, &bytes);
    let mut values = BTreeMap::new();
    values.insert("baseUrl".to_owned(), json!(base));
    if let Some(t) = token {
        values.insert("apiToken".to_owned(), json!(t));
    }
    modules.save_settings(&id, &values).unwrap();
    modules.set_enabled(true).unwrap();
    id
}

fn song(id: &str, title: &str, artist: &str, secs: u32) -> Track {
    Track {
        video_id: id.into(),
        title: title.into(),
        artists: artist.into(),
        duration_secs: Some(secs),
        ..Default::default()
    }
}

/// Fails every request: proves that a stream came from the module.
struct NoYouTube;

impl StreamResolver for NoYouTube {
    fn resolve(&self, video_id: &str) -> music_core::Result<ResolvedStream> {
        Err(Error::Unplayable(format!("youtube:{video_id}")))
    }
}

#[test]
fn search_resolve_and_play_a_module_stream_end_to_end() {
    let flac = std::fs::read(fixture("sine_rg.flac")).unwrap();
    // A 7 s WAV (> 1 MiB): progressive, without range support on the server.
    let long_wav = wav(7, 44_100);
    assert!(long_wav.len() > 1024 * 1024);
    let entries = json!([
        catalog_entry(
            "t1",
            "Sine Wave",
            "Test Artist",
            1.0,
            json!([
            {"quality": "lossless", "file": "sine.flac", "mimeType": "audio/flac", "codec": "flac", "sampleRate": 44100},
            {"quality": "compressed", "file": "sine.mp3", "mimeType": "audio/mpeg", "codec": "mp3", "bitrate": 128000}])
        ),
        catalog_entry(
            "t2",
            "Long Tone",
            "Test Artist",
            7.0,
            json!([
            {"quality": "lossless", "file": "long.wav", "mimeType": "audio/wav", "codec": "pcm"}])
        ),
        catalog_entry("t3", "Sine Wave (Live)", "Test Artist", 1.0, json!([])),
    ]);
    let server = echo_server(
        entries,
        vec![
            ("sine.flac", flac.clone(), "audio/flac"),
            (
                "sine.mp3",
                std::fs::read(fixture("sine.mp3")).unwrap(),
                "audio/mpeg",
            ),
            ("long.wav", long_wav.clone(), "audio/wav"),
        ],
        false,
    );
    let dir = temp_dir("modules-e2e");
    let modules = open(&dir);
    let id = install_echo(&modules, &server.base, Some("s3cret"));

    // Search through the module.
    let found = modules.search(&id, "sine wave test artist").unwrap();
    assert_eq!(found.len(), 2, "{found:?}");

    // Resolve a YouTube song: best acceptable match (the live version is rejected).
    let track = song("yt-sine", "Sine Wave", "Test Artist", 1);
    let selection = modules.resolve(&track).expect("resolved");
    assert_eq!(selection.candidate.id, "t1");
    assert_eq!(selection.stream.mime_type, "audio/flac");
    assert_eq!(selection.stream.sample_rate, Some(44_100));
    assert!(
        selection
            .stream
            .headers
            .contains(&("Authorization".into(), "Bearer s3cret".into()))
    );
    let playback = modules.health(&id).playback.unwrap();
    assert_eq!(playback.status, CheckStatus::Resolved);
    assert_eq!(playback.candidates, 2);

    // Full decode through the player, headless; YouTube would fail.
    let resolver = Arc::new(ModuleResolver::new(modules.clone(), Arc::new(NoYouTube)));
    for (track, secs, codec) in [
        (track.clone(), 1.0, "FLAC 16/44.1"),
        (
            song("yt-long", "Long Tone", "Test Artist", 7),
            7.0,
            "PCM 16/44.1",
        ),
    ] {
        let (player, mut output) = Player::headless(resolver.clone(), 48_000).unwrap();
        player.set_volume(0.0);
        player.play(track.clone());
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut furthest = Duration::ZERO;
        let mut duration = None;
        let mut source = None;
        loop {
            // ~3x real time, so the engine's 100 ms ticks see the playback.
            for _ in 0..960 {
                output.next();
            }
            std::thread::sleep(Duration::from_millis(3));
            let state = player.state();
            if state.source.is_some() {
                source = state.source.clone();
            }
            if state.status == PlaybackStatus::Playing {
                furthest = furthest.max(state.position);
                duration = state.duration.or(duration);
            }
            if let PlaybackStatus::Error(e) = &state.status {
                panic!("{}: {e}", track.title);
            }
            if state.status == PlaybackStatus::Ended {
                assert!(!state.preview_only);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{} never ended: {state:?}",
                track.title
            );
        }
        let source = source.expect("source reported");
        assert_eq!(source.kind, SourceKind::Module);
        assert_eq!(source.provider, "Echo (test provider)");
        assert_eq!(source.module_id.as_deref(), Some(id.as_str()));
        assert_eq!(source.quality_label().as_deref(), Some(codec));
        let played = furthest.as_secs_f64();
        assert!(
            played >= secs * 0.9,
            "{}: played {played:.2} s of {secs} s",
            track.title
        );
        let total = duration.expect("duration").as_secs_f64();
        assert!(
            (total - secs).abs() < 0.1,
            "{}: duration {total}",
            track.title
        );
    }
    // The audio requests carried the module's headers and no Range (server has none).
    let audio = server.requests("/audio/");
    assert!(
        audio
            .iter()
            .all(|r| r.headers.get("authorization").map(String::as_str) == Some("Bearer s3cret"))
    );
    assert!(audio.iter().all(|r| !r.headers.contains_key("range")));

    // The secret never reached the registry file.
    let registry = std::fs::read_to_string(dir.join("modules/registry.json")).unwrap();
    assert!(!registry.contains("s3cret"));
    let secrets = std::fs::read_to_string(dir.join("secrets.json")).unwrap();
    assert!(secrets.contains("s3cret"));
    assert_eq!(modules.settings(&id).unwrap()["apiToken"], json!("s3cret"));

    // Quality select → the compressed variant.
    let mut values = modules.settings(&id).unwrap();
    values.insert("quality".into(), json!("compressed"));
    modules.save_settings(&id, &values).unwrap();
    let selection = modules.resolve(&track).unwrap();
    assert_eq!(selection.stream.mime_type, "audio/mpeg");
    assert_eq!(
        selection
            .to_resolved()
            .source
            .unwrap()
            .quality_label()
            .as_deref(),
        Some("MP3 128 kbps")
    );

    // Invalid settings are refused.
    values.insert("searchLimit".into(), json!(500));
    assert_eq!(
        modules.save_settings(&id, &values).unwrap_err().failure,
        Failure::InvalidConfiguration
    );

    // Downloads go through the module too, as a complete file.
    let downloads =
        music_core::downloads::DownloadManager::new(dir.join("media"), resolver.clone(), None)
            .unwrap();
    downloads.download(song("yt-dl", "Long Tone", "Test Artist", 7));
    let deadline = Instant::now() + Duration::from_secs(20);
    let path = loop {
        if let Some(path) = downloads.local_file("yt-dl") {
            break path;
        }
        assert!(
            Instant::now() < deadline,
            "download never finished: {:?}",
            downloads.states()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(path.extension().unwrap(), "wav");
    assert_eq!(std::fs::read(&path).unwrap(), long_wav);
}

#[test]
fn resolver_chaining_and_fallback() {
    let flac = std::fs::read(fixture("sine_rg.flac")).unwrap();
    let entries = json!([catalog_entry(
        "t1",
        "Sine Wave",
        "Test Artist",
        1.0,
        json!([
        {"quality": "lossless", "file": "sine.flac", "mimeType": "audio/flac", "codec": "flac"}])
    )]);
    let server = echo_server(entries, vec![("sine.flac", flac, "audio/flac")], true);
    let dir = temp_dir("modules-chain");
    let modules = open(&dir);
    // A broken module first, then echo: priority order is followed.
    let broken = install(&modules, &probe("broken", "throw new Error('down');"));
    let echo = install_echo(&modules, &server.base, None);
    let resolver = ModuleResolver::new(modules.clone(), Arc::new(NoYouTube));
    let hit = song("v1", "Sine Wave", "Test Artist", 1);
    let miss = song("v2", "Something Else", "Nobody", 200);

    let stream = resolver.resolve_track(&hit, Quality::High).unwrap();
    assert!(stream.progressive);
    assert_eq!(
        stream.source.as_ref().unwrap().module_id.as_deref(),
        Some(echo.as_str())
    );
    assert_eq!(
        modules.health(&broken).playback.unwrap().status,
        CheckStatus::Failed
    );

    // No match: falls back to YouTube (here: the failing stand-in).
    let err = resolver
        .resolve_track(&miss, Quality::High)
        .unwrap_err()
        .to_string();
    assert!(err.contains("youtube:v2"), "{err}");
    assert_eq!(
        modules.health(&echo).playback.unwrap().status,
        CheckStatus::NoMatch
    );

    // Modules only: no fallback.
    modules.set_fallback_to_youtube(false).unwrap();
    let err = resolver
        .resolve_track(&song("v3", "Other", "Nobody", 10), Quality::High)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no enabled module"), "{err}");
    modules.set_fallback_to_youtube(true).unwrap();

    // Disabled module / modules off / local files: straight to YouTube.
    modules.set_module_enabled(&echo, false).unwrap();
    let err = resolver
        .resolve_track(&song("v4", "Sine Wave", "Test Artist", 1), Quality::High)
        .unwrap_err()
        .to_string();
    assert!(err.contains("youtube:v4"), "{err}");
    modules.set_module_enabled(&echo, true).unwrap();
    modules.set_enabled(false).unwrap();
    let err = resolver
        .resolve_track(&song("v5", "Sine Wave", "Test Artist", 1), Quality::High)
        .unwrap_err()
        .to_string();
    assert!(err.contains("youtube:v5"), "{err}");
    modules.set_enabled(true).unwrap();
    let local = Track {
        video_id: "local:abc".into(),
        title: "Sine Wave".into(),
        artists: "Test Artist".into(),
        local_path: Some(fixture("sine_rg.flac")),
        ..Default::default()
    };
    assert!(modules.resolve(&local).is_none());

    // Reordering puts echo first; removing the broken one cleans it up.
    modules.move_module(&echo, -1).unwrap();
    assert_eq!(modules.registry().modules[0].id, echo);
    modules.remove(&broken).unwrap();
    assert_eq!(modules.registry().modules.len(), 1);

    // A health check.
    assert_eq!(modules.check(&echo).status, CheckStatus::Ready);
    // A required setting missing → configuration status.
    let mut values = modules.settings(&echo).unwrap();
    values.insert("baseUrl".into(), Value::Null);
    assert!(modules.save_settings(&echo, &values).is_err(), "required");

    // Everything survives a reopen.
    drop(resolver);
    let reopened = open(&dir);
    let registry = reopened.registry();
    assert!(registry.enabled);
    assert_eq!(registry.modules[0].manifest.id, "dev.music-macos.echo");
    assert_eq!(
        reopened.settings(&echo).unwrap()["baseUrl"],
        json!(server.base)
    );
}

#[test]
fn installs_from_urls_descriptors_and_catalogs_with_updates() {
    let v1 = std::fs::read(fixture("modules/echo.arch")).unwrap();
    // Version 2: same id, different script.
    let dir2 = temp_dir("echo-v2");
    for name in ["provider.js", "icon.png"] {
        std::fs::copy(echo_dir().join(name), dir2.join(name)).unwrap();
    }
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(echo_dir().join("manifest.json")).unwrap()).unwrap();
    manifest["module"]["version"] = json!("2.0.0");
    std::fs::write(dir2.join("manifest.json"), manifest.to_string()).unwrap();
    let mut script = std::fs::read_to_string(dir2.join("provider.js")).unwrap();
    script.push_str("\n// v2\n");
    std::fs::write(dir2.join("provider.js"), script).unwrap();
    let v2 = package::pack_dir(&dir2).unwrap();

    let current = Arc::new(std::sync::Mutex::new(v1.clone()));
    let served = current.clone();
    let (sha1, sha2) = (sha(&v1), sha(&v2));
    let server = Server::start(move |req: &Req| match req.path.as_str() {
        "/echo.arch" => Resp::ok("application/octet-stream", served.lock().unwrap().clone()),
        "/latest.json" => Resp::json(&json!({"download": "echo.arch"})),
        "/loop.json" => Resp::json(&json!({"download": "loop.json"})),
        "/catalog.json" => Resp::json(&json!({"name": "Test catalog", "modules": [
            {"id": "dev.music-macos.echo", "name": "Echo", "version": "1.0.0", "download": "./echo.arch", "sha256": sha1},
            {"id": "other", "name": "Other", "download": "./missing.arch", "type": "THEME"}],
            "external_sources": [{"url": "more.json"}]})),
        "/more.json" => Resp::json(
            &json!({"modules": [{"id": "x.more", "name": "More", "download": "nested/x.arch"}]}),
        ),
        "/page/" => Resp::ok(
            "text/html",
            "<html><a href='../catalog.json'>catalog</a></html>",
        ),
        "/bad.arch" => Resp::ok("application/octet-stream", b"PK\x03\x04broken".to_vec()),
        _ => Resp::status(404),
    });
    let _ = sha2;
    let dir = temp_dir("modules-urls");
    let modules = open(&dir);

    // Direct URL.
    let Import::Module(pending) = modules
        .import_url(&format!("{}/echo.arch", server.base))
        .unwrap()
    else {
        panic!("module expected")
    };
    assert_eq!(pending.manifest.version, "1.0.0");
    assert_eq!(pending.update_url, format!("{}/echo.arch", server.base));
    let id = modules.commit(Import::Module(pending)).unwrap();
    assert!(modules.check_update(&id).unwrap().is_none(), "up to date");
    *current.lock().unwrap() = v2.clone();
    let update = modules.check_update(&id).unwrap().expect("an update");
    assert_eq!(update.manifest.version, "2.0.0");
    assert_eq!(update.replacement.as_deref(), Some(id.as_str()));
    assert_eq!(
        modules.commit(Import::Module(Box::new(update))).unwrap(),
        id,
        "same installation"
    );
    assert_eq!(modules.module(&id).unwrap().manifest.version, "2.0.0");
    *current.lock().unwrap() = v1.clone();

    // A descriptor: the update URL stays the descriptor.
    let Import::Module(pending) = modules
        .import_url(&format!("{}/latest.json", server.base))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(pending.update_url, format!("{}/latest.json", server.base));
    assert!(
        modules
            .import_url(&format!("{}/loop.json", server.base))
            .is_err(),
        "descriptor loops are rejected"
    );

    // A catalog (also found through an HTML page), installing an entry with its checksum.
    let Import::Catalog(catalog) = modules
        .import_url(&format!("{}/page/", server.base))
        .unwrap()
    else {
        panic!("catalog expected")
    };
    assert_eq!(catalog.name, "Test catalog");
    let ids: Vec<&str> = catalog.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["dev.music-macos.echo", "other", "x.more"]);
    assert_eq!(
        catalog.entries[2].download.as_deref(),
        Some(format!("{}/nested/x.arch", server.base).as_str())
    );
    let catalog_id = modules.commit(Import::Catalog(catalog.clone())).unwrap();
    assert_eq!(modules.registry().catalogs.len(), 1);
    let entry = &catalog.entries[0];
    assert!(matches!(
        modules.import_catalog_entry(entry).unwrap(),
        Import::Module(_)
    ));
    *current.lock().unwrap() = v2;
    let e = modules.import_catalog_entry(entry).unwrap_err();
    assert_eq!(e.failure, Failure::Integrity, "catalog checksum enforced");
    modules.refresh_catalog(&catalog_id).unwrap();
    modules.remove_catalog(&catalog_id).unwrap();
    assert!(modules.registry().catalogs.is_empty());

    // Download errors and broken packages.
    assert_eq!(
        modules
            .import_url(&format!("{}/nope.arch", server.base))
            .unwrap_err()
            .code
            .as_deref(),
        Some("HTTP_404")
    );
    assert_eq!(
        modules
            .import_url(&format!("{}/bad.arch", server.base))
            .unwrap_err()
            .code
            .as_deref(),
        Some("ARCH_ZIP")
    );
    assert!(modules.import_url("file:///etc/passwd").is_err());
    // A file import has no update URL.
    let file_id = {
        let path = dir.join("echo.arch");
        std::fs::write(&path, &v1).unwrap();
        let Import::Module(p) = modules.import_file(&path).unwrap() else {
            panic!()
        };
        assert!(p.update_url.is_empty());
        modules.commit(Import::Module(p)).unwrap()
    };
    assert_ne!(
        file_id, id,
        "a file import is a separate installation from the URL one"
    );
    assert!(modules.check_update(&file_id).unwrap().is_none());
}
