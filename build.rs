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
//! - **Windows**: compile the `upxz-winstub` crate (temp-file +
//!   `CreateProcessW` self-extractor) and copy its `.exe` into
//!   `OUT_DIR/upxz-winstub.bin`. Embedded by `src/sfx.rs::windows_stub_bytes()`.
//!   The Windows SFX mirrors the Linux layout
//!   `[stub][.upxz][trailer: u64 stub_size BE]`; the stub writes the restored
//!   PE to `%TEMP%` and execs it (Windows has no portable in-memory exec —
//!   see `winstub/src/main.rs` module docs for the NT-section PoC discussion).
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
        "windows" => build_windows_stub(&out_dir),
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

    // Empty placeholders for the other platforms so `include_bytes!` compiles
    // on every target. The accessors return None when these are empty.
    std::fs::write(&loader_path, b"").expect("write empty loader placeholder");
    std::fs::write(out_dir.join("upxz-winstub.bin"), b"").expect("write empty winstub placeholder");

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

    // Empty placeholders for the other platforms so `include_bytes!` compiles.
    std::fs::write(&stub_path, b"").expect("write empty stub placeholder");
    std::fs::write(out_dir.join("upxz-winstub.bin"), b"").expect("write empty winstub placeholder");

    println!("cargo:rerun-if-changed=loader/src/main.rs");
    println!("cargo:rerun-if-changed=loader/Cargo.toml");
    println!("cargo:rerun-if-changed={}", artifact.display());
}

/// Windows: build the `upxz-winstub` workspace member (temp-file +
/// `CreateProcessW` self-extractor) and copy its `.exe` into
/// `OUT_DIR/upxz-winstub.bin`. Leaves the Linux stub and macOS loader
/// placeholders empty.
///
/// The winstub is a normal workspace member (unlike the macOS loader, which is
/// standalone for the no_std + `panic=abort` reason). We still shell out to
/// `cargo build -p upxz-winstub` for its *bytes*, exactly like the Linux stub
/// branch — Cargo does not otherwise expose a sibling's built artifact to a
/// build script.
fn build_windows_stub(out_dir: &Path) {
    let stub_path = out_dir.join("upxz-winstub.bin");
    let linux_stub_path = out_dir.join("upxz-stub.bin");
    let loader_path = out_dir.join("upxz-loader.bin");

    // Build the winstub as a release artifact targeting the TARGET triple
    // (NOT HOST). When upxz is cross-compiled to Windows from a non-Windows
    // host (e.g. `cargo build --target x86_64-pc-windows-gnu` on macOS/Linux),
    // HOST is the macOS/Linux triple and TARGET is the Windows triple; we must
    // build the winstub for the Windows target so it can be embedded into a
    // Windows-packed file. `TARGET` is the env var Cargo sets to the triple
    // being built for. (The sibling Linux `build_linux_stub` uses HOST because
    // it is only reached when target_os == linux, where HOST == TARGET in the
    // common native-build case; we use TARGET here to support cross-compile.)
    let target = env::var("TARGET").expect("TARGET set by Cargo");

    // Use a SEPARATE target dir for the recursive winstub build. Cargo build
    // scripts that recursively invoke `cargo build` against the SAME workspace
    // can deadlock on the workspace package lock (the child waits for the lock
    // the parent already holds). Redirecting the child's output to an isolated
    // dir (under OUT_DIR, which is unique per build) sidesteps the lock
    // entirely. This is the documented workaround for build-script recursion.
    let stub_target_dir = out_dir.join("winstub-target");
    std::fs::create_dir_all(&stub_target_dir)
        .expect("create isolated target dir for winstub build");

    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
        .args([
            "build",
            "--release",
            "-p",
            "upxz-winstub",
            "--target",
            &target,
        ])
        // Isolate the recursive build's target dir so it does NOT share the
        // parent workspace's package lock (avoids the build-script recursion
        // deadlock). The artifact lands under this dir.
        .env("CARGO_TARGET_DIR", &stub_target_dir)
        .status()
        .expect("failed to invoke `cargo build -p upxz-winstub`");
    if !status.success() {
        panic!("building upxz-winstub failed (exit {:?})", status.code());
    }

    // Locate the produced binary. With an explicit `--target` AND a custom
    // CARGO_TARGET_DIR, the artifact is under
    // `<stub_target_dir>/<triple>/release/upxz-winstub(.exe)`. The winstub
    // binary has a `.exe` suffix when the TARGET is Windows (the only case
    // this branch is reached), regardless of the host.
    let artifact = stub_target_dir
        .join(&target)
        .join("release")
        .join(winstub_binary_name());
    if !artifact.is_file() {
        panic!(
            "upxz-winstub binary not found at {} after build",
            artifact.display()
        );
    }
    std::fs::copy(&artifact, &stub_path).expect("copy winstub to OUT_DIR");

    // Empty placeholders for the other platforms so `include_bytes!` compiles.
    std::fs::write(&linux_stub_path, b"").expect("write empty stub placeholder");
    std::fs::write(&loader_path, b"").expect("write empty loader placeholder");

    println!("cargo:rerun-if-changed=winstub/src/main.rs");
    println!("cargo:rerun-if-changed=winstub/Cargo.toml");
    println!("cargo:rerun-if-changed={}", artifact.display());
}

/// Non-Linux/non-macOS/non-Windows targets: emit empty placeholders for all
/// three pieces so `include_bytes!` still compiles. The accessors return
/// `None`, and `upxz -c` refuses with a clear runtime message.
fn emit_empty_placeholders(out_dir: &Path) {
    std::fs::write(out_dir.join("upxz-stub.bin"), b"").expect("write empty stub placeholder");
    std::fs::write(out_dir.join("upxz-loader.bin"), b"").expect("write empty loader placeholder");
    std::fs::write(out_dir.join("upxz-winstub.bin"), b"").expect("write empty winstub placeholder");
}

fn stub_binary_name() -> &'static str {
    "upxz-stub"
}

fn loader_binary_name() -> &'static str {
    "upxz-loader"
}

/// The winstub binary file name as cargo writes it for the TARGET. Cargo
/// appends `.exe` when the *target* is Windows; this function is only reached
/// from `build_windows_stub` (i.e. `CARGO_CFG_TARGET_OS == "windows"`), so the
/// `.exe` form is what lands on disk. We still check the env var rather than
/// `cfg!` so a non-Windows HOST cross-compiling to Windows picks the right
/// name (cargo uses the target triple to decide the suffix).
fn winstub_binary_name() -> String {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        "upxz-winstub.exe".to_owned()
    } else {
        "upxz-winstub".to_owned()
    }
}
