//! Process-wide memory tuning (macOS).
//!
//! The macOS allocator keeps freed memory for reuse instead of handing it
//! back: freed blocks of 1 KiB and up stay dirty (and count towards the
//! process footprint that Activity Monitor shows) until the same sizes are
//! allocated again, and freed blocks of ~128 KiB and up sit in a cache that
//! is never trimmed. A process that once needed a lot — parsing a 4 MB player
//! script in the challenge solver, decoding artwork, parsing large InnerTube
//! responses — keeps that peak for the rest of its life, which made the
//! footprint vary by 100–150 MB between identical runs.
//!
//! `MallocSpaceEfficient=1` switches libmalloc to returning free pages to the
//! system promptly. It is read once, when the process starts, so the app
//! re-executes itself with the variable set (≈5 ms, before any thread exists);
//! the macOS bundle sets it in `Info.plist` (`LSEnvironment`) so that a normal
//! launch needs no re-exec. `MUSIC_MALLOC=system` keeps the default allocator
//! behaviour (for comparisons).

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
