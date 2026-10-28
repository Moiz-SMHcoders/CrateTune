//! Process-wide memory tuning (macOS).
//!
//! The macOS allocator (measured on macOS 26) keeps freed memory for reuse
//! instead of handing it back, and it all counts towards the footprint that
//! Activity Monitor shows: of 200 MB freed in 1–32 KiB blocks, 80–115 MB
//! stay dirty; freed blocks of ~128 KiB and more (tested up to 32 MB) stay
//! completely, in a cache that is not trimmed — not by `malloc_zone_pressure_relief`
//! either, and destroying a private zone doesn't return what was freed in it.
//! So the process kept every peak: preprocessing the 4 MB player script in the
//! challenge solver (QuickJS churns through hundreds of MB), decoding artwork,
//! parsing InnerTube responses. That is why the footprint varied by 100–150 MB
//! between identical runs.
//!
//! `MallocSpaceEfficient=1` makes libmalloc return free pages to the system
//! promptly (the same test keeps 2 MB). It is read once, when the process
//! starts, so the app re-executes itself with the variable set (≈10 ms, before
//! any thread exists); the macOS bundle sets it in `Info.plist`
//! (`LSEnvironment`) so that a launch from Finder needs no re-exec. Drawing a
//! frame got no slower (`@bench`: 0.8 ms instead of 1.2 ms per Now Playing
//! frame). `MUSIC_MALLOC=system` keeps the default behaviour (comparisons).

/// Re-executes the process with the space-efficient allocator unless it is
/// already active (or opted out). Call first thing in `main`.
#[cfg(target_os = "macos")]
pub fn tune_allocator() {
    use std::os::unix::process::CommandExt as _;

    const VAR: &str = "MallocSpaceEfficient";
    if std::env::var_os(VAR).is_some() || std::env::var("MUSIC_MALLOC").is_ok_and(|v| v == "system")
    {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut args = std::env::args_os();
    let arg0 = args.next().unwrap_or_else(|| exe.clone().into_os_string());
    // Only returns if the exec failed; then run with the default allocator.
    let err = std::process::Command::new(&exe)
        .arg0(arg0)
        .args(args)
        .env(VAR, "1")
        .exec();
    eprintln!("music-gpui: re-exec with {VAR}=1 failed: {err}");
}

#[cfg(not(target_os = "macos"))]
pub fn tune_allocator() {}
