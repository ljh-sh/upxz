//! Embedded Linux SFX stub bytes, baked in at build time by `build.rs`.
//!
//! `build.rs` compiles the `upxz-stub` crate (Linux only) and copies its
//! binary into `OUT_DIR/upxz-stub.bin`. We `include_bytes!` it here. On
//! non-Linux targets the file is an empty placeholder and `stub_bytes()`
//! returns `None`, so `upxz -c` fails with a clear message at runtime
//! rather than emitting a broken SFX.

/// Return the embedded stub bytes, or `None` when upxz was built on a target
/// without the Linux SFX stub.
pub fn stub_bytes() -> Option<&'static [u8]> {
    // OUT_DIR is set at build time; the path is fixed relative to OUT_DIR.
    // include_bytes! requires a literal path, so we use the env! macro value
    // via a build-time const concatenation. We read the OUT_DIR-relative path
    // emitted by build.rs.
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upxz-stub.bin"));
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}
