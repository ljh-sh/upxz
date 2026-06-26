//! Embedded SFX pieces (Linux stub + macOS boot/loader), baked in at build
//! time by `build.rs`.
//!
//! `build.rs` compiles the platform-specific SFX components and copies them
//! into `OUT_DIR`:
//!   - Linux: `upxz-stub.bin` — the memfd+fexecve self-extractor.
//!   - macOS: `upxz-boot.sh` + `upxz-loader.bin` — the three-segment
//!     `[boot][loader][.upxz][trailer]` SFX.
//!
//! On targets where a given piece was not built, `build.rs` writes an empty
//! placeholder so `include_bytes!` still compiles, and the accessor below
//! returns `None`. `upxz -c` then refuses with a clear message at runtime
//! rather than emitting a broken SFX.

/// Return the embedded Linux stub bytes, or `None` when upxz was built on a
/// target without the Linux SFX stub.
pub fn stub_bytes() -> Option<&'static [u8]> {
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upxz-stub.bin"));
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

/// Return the embedded macOS boot script bytes, or `None` when upxz was not
/// built on macOS.
pub fn macos_boot_bytes() -> Option<&'static [u8]> {
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upxz-boot.sh"));
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

/// Return the embedded macOS upxz-loader binary bytes, or `None` when upxz was
/// not built on macOS.
pub fn macos_loader_bytes() -> Option<&'static [u8]> {
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upxz-loader.bin"));
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}
