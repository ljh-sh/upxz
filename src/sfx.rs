//! Embedded SFX pieces (Linux stub + macOS loader), baked in at build time by
//! `build.rs`.
//!
//! `build.rs` compiles the platform-specific SFX components and copies them
//! into `OUT_DIR`:
//!   - Linux: `upxz-stub.bin` — the memfd+fexecve self-extractor.
//!   - macOS: `upxz-loader.bin` — the two-segment
//!     `[loader][.upxz][trailer]` SFX loader. The loader binary IS the packed
//!     file's Mach-O header.
//!
//! On targets where a given piece was not built, `build.rs` writes an empty
//! placeholder so `include_bytes!` still compiles, and the accessor below
//! returns `None`. `upxz -c` then refuses with a clear message at runtime
//! rather than emitting a broken SFX.

/// Return the embedded Linux stub bytes, or `None` when upxz was built on a
/// target without the Linux SFX stub.
///
/// Only called from the Linux SFX packer (`pack_sfx_linux`); on other targets
/// the function is dead code, hence the allow.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn stub_bytes() -> Option<&'static [u8]> {
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upxz-stub.bin"));
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

/// Return the embedded macOS upxz-loader binary bytes, or `None` when upxz is
/// not built on macOS. In the two-segment design this loader is the packed
/// file's Mach-O header.
///
/// Only called from the macOS SFX packer (`pack_sfx_macos`); on other targets
/// the function is dead code, hence the allow.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn macos_loader_bytes() -> Option<&'static [u8]> {
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upxz-loader.bin"));
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

/// Return the embedded Windows stub bytes (`upxz-winstub.exe`), or `None`
/// when upxz was not built on/for Windows. The Windows SFX layout mirrors the
/// Linux stub (`[stub][.upxz][trailer: u64 stub_size BE]`); the stub writes
/// the restored PE to `%TEMP%` and `CreateProcessW`s it. Windows has no
/// portable in-memory exec — the NT-section route is documented but not
/// compiled; see `winstub/src/main.rs` module docs.
///
/// Only called from the Windows SFX packer (`pack_sfx_windows`); on other
/// targets the function is dead code, hence the allow.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn windows_stub_bytes() -> Option<&'static [u8]> {
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upxz-winstub.bin"));
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}
