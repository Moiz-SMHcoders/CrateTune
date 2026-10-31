//! Process-wide memory tuning (macOS), and the memory figures the perf scripts
//! log ([`usage`], `@mem`).
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

/// Hands memory freed in bulk (artwork dropped from the cache) back to the system
/// a moment later, once per burst. glibc keeps freed blocks for reuse, like the
/// macOS allocator without `MallocSpaceEfficient` (above): scrolling through a
/// long Home on Linux left ~370 MB resident with 48 MB of artwork in the cache
/// (perf-frames.sh `--page-art`). Elsewhere a no-op.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn trim_soon(cx: &gpui::App) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static PENDING: AtomicBool = AtomicBool::new(false);
    if PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    let executor = cx.background_executor().clone();
    cx.background_executor()
        .spawn(async move {
            executor.timer(std::time::Duration::from_secs(2)).await;
            PENDING.store(false, Ordering::Release);
            // SAFETY: plain libc call; returns whether memory was released.
            unsafe { libc::malloc_trim(0) };
        })
        .detach();
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn trim_soon(_: &gpui::App) {}

/// What the process uses now, as `(name, bytes)` pairs, for `@mem` (the perf
/// scripts log it per platform, so runs on each OS compare like for like):
///
/// * macOS: `footprint` (what Activity Monitor shows as Memory; GPU textures and
///   drawables count in it, the GPU shares the memory), its peak, the resident size;
/// * Windows: private bytes (Task Manager's Memory column), working set and its peak;
/// * Linux: resident set, its peak (`VmHWM`), and the anonymous part of it.
pub fn usage() -> Vec<(&'static str, u64)> {
    platform_usage().unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn platform_usage() -> Option<Vec<(&'static str, u64)>> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `info` is a `rusage_info_v4`, the flavour asked for.
    let status = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: filled in by the successful call above.
    let info = unsafe { info.assume_init() };
    Some(vec![
        ("footprint", info.ri_phys_footprint),
        ("peak", info.ri_lifetime_max_phys_footprint),
        ("resident", info.ri_resident_size),
    ])
}

#[cfg(target_os = "windows")]
fn platform_usage() -> Option<Vec<(&'static str, u64)>> {
    use windows::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the EX struct starts with the plain one; `cb` says which it is.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    ok.as_bool().then(|| {
        vec![
            ("private", counters.PrivateUsage as u64),
            ("working set", counters.WorkingSetSize as u64), // i18n-ignore
            ("peak working set", counters.PeakWorkingSetSize as u64), // i18n-ignore
        ]
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_usage() -> Option<Vec<(&'static str, u64)>> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status.lines().find_map(|line| {
            let kb = line.strip_prefix(name)?.strip_prefix(':')?;
            let kb: u64 = kb.trim().trim_end_matches("kB").trim().parse().ok()?;
            Some(kb * 1024)
        })
    };
    Some(vec![
        ("resident", field("VmRSS")?),
        ("peak", field("VmHWM")?),
        ("anonymous", field("RssAnon")?),
    ])
}
