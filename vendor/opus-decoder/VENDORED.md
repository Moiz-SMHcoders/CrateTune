# opus-decoder 0.1.1 (vendored, patched)

Pure-Rust Opus decoder by the Rusopus authors — <https://github.com/TadeuszWolfGang/Rusopus>,
<https://crates.io/crates/opus-decoder> — licensed **MIT OR Apache-2.0** (see the crate's
`Cargo.toml`; the published package ships no license files, the texts are at
<https://opensource.org/license/mit> and <https://www.apache.org/licenses/LICENSE-2.0>).

Copied from the crates.io package 0.1.1 and used through `[patch.crates-io]` in the
workspace `Cargo.toml`. CrateTune's changes (search for "CrateTune patch"):

1. `src/celt/kiss_fft.rs` — `flat_fft_forward` (the FFT inside the CELT inverse MDCT)
   was a direct O(n²) DFT computing a `sin_cos` per term; it now runs a cached
   `rustfft` plan. A test compares it with the original DFT (kept as
   `flat_dft_reference`).
2. `src/lib.rs` + every `// #region agent log` block — the published code opened its
   developer's debug log (`/Users/tadeusz/Opus/Rasopus/.cursor/debug-bea564.log`,
   `./.cursor/debug-bea564.log`) for append on decode paths, some per packet. The
   `std::fs::OpenOptions::new()` calls are replaced by `crate::NoDebugLog`, which never
   opens anything.
3. `Cargo.toml` — `rustfft` added; the `criterion` dev-dependency removed (benches are
   not in the package). The RFC conformance tests need `testdata/`, which the package
   excludes.

Drop this directory once an upstream release fixes both.
