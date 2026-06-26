//! Build script: bake platform-specific SFX pieces into the `upxz` packer.
//!
//! - **Linux**: compile the `upxz-stub` crate (memfd + fexecve self-extractor)
//!   and copy its binary into `OUT_DIR/upxz-stub.bin`. Embedded by
//!   `src/sfx.rs::stub_bytes()`.
//! - **macOS**: build the standalone `upxz-loader` crate (`loader/`, no_std +
//!   zstd-sys FFI) and copy its binary into `OUT_DIR/upxz-loader.bin`. The
//!   macOS SFX is two-segment `[loader][.upxz][trailer]`; the loader binary is
//!   the packed file's Mach-O header. Embedded by
//!   `src/sfx.rs::macos_loader_bytes()`.
//! - **Other targets**: emit empty placeholders so `include_bytes!` still
//!   compiles, and `upxz -c` refuses with a clear message at runtime.
//!
//! Why we shell out to `cargo build` rather than relying on the workspace
//! auto-build: we need the *bytes* of the stub/loader binaries, not the source
//! of a sibling crate — Cargo does not otherwise expose a sibling's built
//! artifact to a build script. This is a standard pattern for
//! self-extracting-binary tooling.
//!
//! The macOS loader is built as a fully standalone crate (it carries its own
//! `[profile.release]` with `panic = "abort"`, required for the no_std
//! panic-handler path; workspace member profiles would be ignored). We invoke
//! `cargo build` from inside `loader/` so that crate's own manifest and
//! profile apply.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by Cargo"));

    match target_os.as_str() {
        "linux" => build_linux_stub(&out_dir),
        "macos" => build_macos_pieces(&out_dir),
        _ => emit_empty_placeholders(&out_dir),
    }

    // Common rerun triggers.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
}

/// Linux: build `upxz-stub` (workspace member) and copy its binary into
/// `OUT_DIR/upxz-stub.bin`. Leaves the macOS loader placeholder empty.
fn build_linux_stub(out_dir: &Path) {
    let stub_path = out_dir.join("upxz-stub.bin");
    let loader_path = out_dir.join("upxz-loader.bin");

    // Build the stub as a release artifact targeting the host triple. We pin
    // release because the stub runs on every SFX invocation: size + speed
    // matter, and a stripped optimized stub is much smaller than a debug one.
    let target = env::var("HOST").expect("HOST set by Cargo");
    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
        .args(["build", "--release", "-p", "upxz-stub", "--target", &target])
        .status()
        .expect("failed to invoke `cargo build -p upxz-stub`");
    if !status.success() {
        panic!("building upxz-stub failed (exit {:?})", status.code());
    }

    // Locate the produced binary. With an explicit `--target`, the artifact is
    // under `target/<triple>/release/`. Without one it is under `target/release/`.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let artifact = manifest_dir
        .join("target")
        .join(&target)
        .join("release")
        .join(stub_binary_name());
    if !artifact.is_file() {
        // Fall back to the no-target path in case the caller overrode target-dir.
        let alt = manifest_dir
            .join("target")
            .join("release")
            .join(stub_binary_name());
        if !alt.is_file() {
            panic!(
                "upxz-stub binary not found at {} (nor {})",
                artifact.display(),
                alt.display()
            );
        }
        std::fs::copy(&alt, &stub_path).expect("copy stub to OUT_DIR");
    } else {
        std::fs::copy(&artifact, &stub_path).expect("copy stub to OUT_DIR");
    }

    // Empty placeholder for the macOS loader so `include_bytes!` compiles on
    // every target. `macos_loader_bytes()` returns None when this is empty.
    std::fs::write(&loader_path, b"").expect("write empty loader placeholder");

    println!("cargo:rerun-if-changed=stub/src/main.rs");
    println!("cargo:rerun-if-changed=stub/Cargo.toml");
    println!("cargo:rerun-if-changed={}", artifact.display());
}

/// macOS: build the standalone `upxz-loader` crate and copy its binary into
/// `OUT_DIR`. Leaves the Linux stub placeholder empty.
fn build_macos_pieces(out_dir: &Path) {
    let stub_path = out_dir.join("upxz-stub.bin");
    let loader_path = out_dir.join("upxz-loader.bin");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let loader_dir = manifest_dir.join("loader");

    // Build the loader crate as a STANDALONE release artifact. We deliberately
    // do NOT use `-p upxz-loader` from the workspace root: the loader is not a
    // workspace member (it needs its own `[profile.release]` with
    // `panic = "abort"`, which workspace membership would override and break
    // the no_std build). Invoking cargo from inside `loader/` makes that
    // manifest authoritative.
    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
        .args(["build", "--release"])
        .current_dir(&loader_dir)
        .status()
        .expect("failed to invoke `cargo build --release` in loader/");
    if !status.success() {
        panic!("building upxz-loader failed (exit {:?})", status.code());
    }

    // The artifact lands in `loader/target/release/upxz-loader`.
    let artifact = loader_dir
        .join("target")
        .join("release")
        .join(loader_binary_name());
    if !artifact.is_file() {
        panic!(
            "upxz-loader binary not found at {} after build",
            artifact.display()
        );
    }
    std::fs::copy(&artifact, &loader_path).expect("copy loader to OUT_DIR");

    // Empty placeholder for the Linux stub so `include_bytes!` compiles.
    std::fs::write(&stub_path, b"").expect("write empty stub placeholder");

    println!("cargo:rerun-if-changed=loader/src/main.rs");
    println!("cargo:rerun-if-changed=loader/Cargo.toml");
    println!("cargo:rerun-if-changed={}", artifact.display());
}

/// Non-Linux/non-macOS targets: emit empty placeholders for both pieces so
/// `include_bytes!` still compiles. `stub_bytes()` and `macos_loader_bytes()`
/// both return `None`, and `upxz -c` refuses with a clear runtime message.
fn emit_empty_placeholders(out_dir: &Path) {
    std::fs::write(out_dir.join("upxz-stub.bin"), b"").expect("write empty stub placeholder");
    std::fs::write(out_dir.join("upxz-loader.bin"), b"").expect("write empty loader placeholder");
}

fn stub_binary_name() -> &'static str {
    "upxz-stub"
}

fn loader_binary_name() -> &'static str {
    "upxz-loader"
}
