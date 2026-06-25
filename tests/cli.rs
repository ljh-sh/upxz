//! End-to-end CLI test: build the upxz binary once and drive it through
//! `std::process::Command`. This covers the two decisions from
//! ljh-sh/mneme#41 at the level users actually hit:
//!
//! - Decision 1 (no xz2): the only compression the binary performs is zstd,
//!   exercised by packing and unpacking.
//! - Decision 2 (3-tier level): `--fast`, default, and `--best` are all
//!   invoked and must each round-trip.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    // cargo sets CARGO_BIN_EXE_<name> for [[bin]] targets during `cargo test`.
    PathBuf::from(env!("CARGO_BIN_EXE_upxz"))
}

#[derive(Debug)]
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "upxz-cli-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn write(&self, name: &str, body: &[u8]) -> PathBuf {
        let p = self.dir.join(name);
        fs::write(&p, body).unwrap();
        p
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn assert_pack_unpack(args: &[&str], input_body: &[u8]) {
    let sb = Sandbox::new();
    let input = sb.write("in.bin", input_body);
    let packed = sb.dir.join("packed.upxz");

    let mut pack = Command::new(bin());
    pack.arg("pack").arg(&input).arg("-o").arg(&packed);
    pack.args(args);
    let status = pack.status().unwrap();
    assert!(status.success(), "pack failed with args {args:?}");

    // packed container must begin with the upxz magic.
    let packed_bytes = fs::read(&packed).unwrap();
    assert!(packed_bytes.len() >= 8);
    assert_eq!(&packed_bytes[..8], b"UPXZ\x01\x00\x00\x00");

    let restored = sb.dir.join("restored.bin");
    let status = Command::new(bin())
        .arg("unpack")
        .arg(&packed)
        .arg("-o")
        .arg(&restored)
        .status()
        .unwrap();
    assert!(status.success(), "unpack failed");

    assert_eq!(fs::read(&restored).unwrap(), input_body);
}

#[test]
fn default_tier_roundtrips() {
    // Decision 2 default tier: no flag => zstd level 3.
    let body = b"the quick brown fox jumps over the lazy dog\n".repeat(50);
    assert_pack_unpack(&[], &body);
}

#[test]
fn fast_tier_roundtrips() {
    assert_pack_unpack(&["--fast"], &vec![0x41u8; 4096]);
}

#[test]
fn best_tier_roundtrips() {
    assert_pack_unpack(&["--best"], &vec![0x42u8; 4096]);
}

#[test]
fn fast_and_best_are_mutually_exclusive() {
    let sb = Sandbox::new();
    let input = sb.write("in.bin", b"hello");
    let status = Command::new(bin())
        .arg("pack")
        .arg(&input)
        .arg("--fast")
        .arg("--best")
        .status()
        .unwrap();
    // clap must reject the conflict with a non-zero exit.
    assert!(!status.success(), "--fast --best should be rejected");
}

#[test]
fn version_flag_works() {
    let out = Command::new(bin()).arg("--version").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("upxz"), "version output: {s}");
}
