//! Build script: compile the `upxz-stub` Linux SFX stub and embed its binary
//! bytes into the `upxz` packer at compile time, so that `upxz -c` can produce
//! a self-extracting executable without shipping a separate artifact.
//!
//! The stub is only meaningful on Linux (it relies on `memfd_create` + the
//! `/proc/self/exe` self-image convention). On other targets we embed nothing
//! and `upxz -c` refuses with a clear message at runtime.
//!
//! We invoke `cargo build --release -p upxz-stub --target <host>` directly
//! rather than relying on the workspace auto-build, because we need the
//! *bytes of the stub binary*, not the stub's source — and Cargo does not
//! otherwise expose a sibling crate's built artifact to this build script.
//! This is a standard pattern for self-extracting-binary tooling.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Only build the stub on Linux. Non-Linux targets get an empty file so
    // `include_bytes!` still compiles; `upxz -c` checks a cfg gate at runtime.
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "linux" {
        emit_empty_stub();
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by Cargo"));
    let stub_path = out_dir.join("upxz-stub.bin");

    // Build the stub as a release artifact targeting the host triple. We pin
    // release because the stub runs on every SFX invocation: size + speed
    // matter, and a stripped optimized stub is much smaller than a debug one.
    let target = env::var("HOST").expect("HOST set by Cargo");
    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
        .args([
            "build",
            "--release",
            "-p",
            "upxz-stub",
            "--target",
            &target,
        ])
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

    // Rerun if the stub source OR its compiled artifact changes. The artifact
    // watch is essential: the stub binary is rebuilt whenever its profile
    // changes (e.g. opt-level tweak in Cargo.toml) even if stub/src is
    // unchanged, and without this line build.rs would not re-run, would not
    // re-copy the fresh artifact into OUT_DIR, and `include_bytes!` would keep
    // embedding the stale stub. Also watch both Cargo.tomls so profile edits
    // trigger a rebuild.
    println!("cargo:rerun-if-changed=stub/src/main.rs");
    println!("cargo:rerun-if-changed=stub/Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", artifact.display());
}

fn stub_binary_name() -> &'static str {
    // The stub [[bin]] is named `upxz-stub`; on Windows it would be `.exe`.
    "upxz-stub"
}

fn emit_empty_stub() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by Cargo"));
    let stub_path = out_dir.join("upxz-stub.bin");
    std::fs::write(&stub_path, b"").expect("write empty stub placeholder");
    // We write an empty placeholder so `include_bytes!` in src/sfx.rs still
    // compiles. `stub_bytes()` returns `None` for an empty embedded file, and
    // `pack_sfx` then refuses `-c` at runtime with a clear "rebuild on Linux"
    // message — no cfg gate needed.
}
