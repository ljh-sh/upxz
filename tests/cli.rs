//! End-to-end CLI tests for the flat (runner-base) upxz CLI.
//!
//! Drives the compiled `upxz` binary via `assert_cmd` through every mode the
//! new (mneme#41) CLI exposes:
//!
//! - pack  : `upxz <file>`           -> writes `<file>.upxz`
//! - run   : `upxz <file>.upxz`      -> decompress + exec (propagates exit code)
//! - -d    : unpack, byte-for-byte round-trip
//! - -l/-t : list / test, exit 0 + expected fields
//! - level : `--fast`, `-z N`, default — all must produce a valid container
//!
//! The container magic is `UPXZ\x01\x00\x00\x00` (8 bytes).

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("upxz").expect("upxz binary built by cargo test")
}

/// Unique scratch dir per test so pack outputs (`<file>.upxz` next to input)
/// never collide across tests running in parallel.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "upxz-cli-{}-{}-{}",
            tag,
            std::process::id(),
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

    /// Path for a not-yet-existing file (an expected output, e.g. `<in>.upxz`).
    fn expected(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The 8-byte upxz container magic.
const MAGIC: &[u8] = b"UPXZ\x01\x00\x00\x00";

fn assert_has_magic(path: &PathBuf) {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert!(
        bytes.len() >= MAGIC.len(),
        "{} is too small to be a container ({} bytes)",
        path.display(),
        bytes.len()
    );
    assert_eq!(
        &bytes[..MAGIC.len()],
        MAGIC,
        "magic mismatch in {}",
        path.display()
    );
}

// ---------------------------------------------------------------------------
// pack
// ---------------------------------------------------------------------------

#[test]
fn pack_plain_file_writes_upxz_with_magic() {
    let sb = Sandbox::new("pack-magic");
    let input = sb.write("hello.txt", b"hello upxz\n".repeat(64).as_slice());
    let expected = sb.expected("hello.txt.upxz");

    bin().arg(&input).assert().success();

    assert!(expected.is_file(), "{} should exist", expected.display());
    assert_has_magic(&expected);
}

#[test]
fn pack_refuses_double_pack() {
    // A `.upxz` container starts with the magic, so `check_packable_input`
    // must reject it. The runner would otherwise auto-run; but the *pack*
    // code path is taken only for non-magic inputs, and a packed container
    // has the magic, so this hits the double-pack guard.
    let sb = Sandbox::new("double-pack");
    let input = sb.write("in.bin", b"some payload bytes".repeat(32).as_slice());

    // First pack succeeds.
    bin().arg(&input).assert().success();
    let packed = sb.expected("in.bin.upxz");
    assert!(packed.is_file());

    // Second pack on the already-packed file must fail (non-zero exit).
    bin().arg(&packed).assert().failure();
}

#[test]
fn pack_rejects_directory_input() {
    // The default (auto-detect) path reads the head for magic sniffing, then
    // calls `read_input_file` which refuses non-regular files.
    let sb = Sandbox::new("dir-in");
    // the sandbox dir itself is a directory.
    bin().arg(&sb.dir).assert().failure();
}

#[test]
fn pack_overwrites_existing_output_with_force() {
    let sb = Sandbox::new("force");
    let input = sb.write("in.bin", b"data".repeat(64).as_slice());
    let out = sb.expected("in.bin.upxz");

    bin().arg(&input).assert().success();
    assert!(out.is_file());

    // Without -f, re-packing must fail because the output already exists
    // (input is still packable — different bytes here keep it non-magic).
    let input2 = sb.write("in2.bin", b"other".repeat(64).as_slice());
    // produce in2.bin.upxz, then move it next to in.bin as in.bin.upxz to
    // collide with the existing one — simpler: just re-run pack on a fresh
    // file whose output name collides. We instead test -f directly.
    bin().arg(&input2).assert().success();

    // Now exercise -f by re-packing input2: its output already exists.
    bin().arg(&input2).arg("-f").assert().success();
}

// ---------------------------------------------------------------------------
// -d unpack round-trip
// ---------------------------------------------------------------------------

#[test]
fn unpack_roundtrips_bytes() {
    let sb = Sandbox::new("unpack-rt");
    let body = b"the quick brown fox\n".repeat(50);
    let input = sb.write("payload.bin", &body);

    bin().arg(&input).assert().success();
    let packed = sb.expected("payload.bin.upxz");

    // Unpack strips the `.upxz` suffix -> restores to `payload.bin` (which
    // still exists from the original, so use -f to overwrite).
    bin().arg("-d").arg(&packed).arg("-f").assert().success();

    let restored = sb.expected("payload.bin");
    assert_eq!(fs::read(&restored).unwrap(), body);
}

#[test]
fn unpack_preserves_magic_checked_bytes() {
    // Larger, less compressible body to exercise the zstd frame path.
    let sb = Sandbox::new("unpack-big");
    let body: Vec<u8> = (0..8192).map(|i| (i * 31) as u8).collect();
    let input = sb.write("big.bin", &body);

    bin().arg(&input).assert().success();
    let packed = sb.expected("big.bin.upxz");
    assert_has_magic(&packed);

    bin().arg("-d").arg(&packed).arg("-f").assert().success();
    assert_eq!(fs::read(sb.expected("big.bin")).unwrap(), body);
}

// ---------------------------------------------------------------------------
// -l list and -t test
// ---------------------------------------------------------------------------

#[test]
fn list_prints_expected_fields() {
    let sb = Sandbox::new("list");
    let input = sb.write("note.txt", b"hello world\n".repeat(16).as_slice());
    bin().arg(&input).assert().success();
    let packed = sb.expected("note.txt.upxz");

    bin().arg("-l").arg(&packed).assert().success().stdout(
        predicate::str::contains("magic\tUPXZ")
            .and(predicate::str::contains("codec\tzstd"))
            .and(predicate::str::contains("name\tnote.txt"))
            .and(predicate::str::contains("compressed\t"))
            .and(predicate::str::contains("original\t")),
    );
}

#[test]
fn test_reports_ok() {
    let sb = Sandbox::new("test");
    let input = sb.write("data.bin", b"abc".repeat(100).as_slice());
    bin().arg(&input).assert().success();
    let packed = sb.expected("data.bin.upxz");

    bin().arg("-t")
        .arg(&packed)
        .assert()
        .success()
        .stdout(predicate::str::contains("ok\t").and(predicate::str::contains("data.bin")));
}

#[test]
fn test_rejects_non_container() {
    let sb = Sandbox::new("test-bad");
    let input = sb.write("plain.bin", b"definitely not a upxz container");
    bin().arg("-t").arg(&input).assert().failure();
}

// ---------------------------------------------------------------------------
// level tiers all produce a valid, round-trippable container
// ---------------------------------------------------------------------------

fn pack_unpack_roundtrip(sb: &Sandbox, body: &[u8], level_args: &[&str]) {
    let input = sb.write("lvl.bin", body);
    // Pack with the given level flags. Flags MUST precede the positional FILE.
    let mut cmd = bin();
    for a in level_args {
        cmd.arg(a);
    }
    cmd.arg(&input).assert().success();

    let packed = sb.expected("lvl.bin.upxz");
    assert_has_magic(&packed);

    bin().arg("-d").arg(&packed).arg("-f").assert().success();
    assert_eq!(fs::read(sb.expected("lvl.bin")).unwrap(), body);
}

#[test]
fn default_tier_roundtrips() {
    let sb = Sandbox::new("lvl-default");
    pack_unpack_roundtrip(&sb, b"x".repeat(2048).as_slice(), &[]);
}

#[test]
fn fast_tier_roundtrips() {
    let sb = Sandbox::new("lvl-fast");
    pack_unpack_roundtrip(&sb, &vec![0x41u8; 4096], &["--fast"]);
}

#[test]
fn z1_tier_roundtrips() {
    let sb = Sandbox::new("lvl-z1");
    pack_unpack_roundtrip(&sb, &vec![0x42u8; 4096], &["-z", "1"]);
}

#[test]
fn z19_tier_roundtrips() {
    let sb = Sandbox::new("lvl-z19");
    pack_unpack_roundtrip(&sb, &vec![0x43u8; 4096], &["-z", "19"]);
}

#[test]
fn z_overrides_fast() {
    // -z N has highest priority; even with --fast present the container must
    // still be produced and round-trip correctly.
    let sb = Sandbox::new("lvl-zover");
    pack_unpack_roundtrip(&sb, b"y".repeat(2048).as_slice(), &["--fast", "-z", "5"]);
}

// ---------------------------------------------------------------------------
// misc
// ---------------------------------------------------------------------------

#[test]
fn version_flag_works() {
    bin().arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("upxz"));
}

// ---------------------------------------------------------------------------
// run (auto-detect on a `.upxz` container) — kept portable by packing a tiny
// shell script that exits 0. On macOS the restored temp file execs with its
// own shebang; this avoids any native-binary SIGKILL risk.
// ---------------------------------------------------------------------------

#[test]
fn run_executes_packed_script_and_propagates_exit_zero() {
    let sb = Sandbox::new("run-zero");
    // A portable "exit 0" script. `/bin/sh` is available on every unix we
    // target; on non-unix this test is skipped.
    #[cfg(unix)]
    {
        let script = b"#!/bin/sh\nexit 0\n";
        let input = sb.write("ok.sh", script);

        bin().arg(&input).assert().success();
        let packed = sb.expected("ok.sh.upxz");
        assert_has_magic(&packed);

        // Running the container must exec the restored script and propagate
        // its exit code (0 here).
        bin().arg(&packed).assert().success();
    }
    #[cfg(not(unix))]
    {
        let _ = sb; // silence unused warning on non-unix
    }
}

#[test]
fn run_propagates_nonzero_exit_code() {
    let sb = Sandbox::new("run-nonzero");
    #[cfg(unix)]
    {
        // `exit 7` — a distinctive code we can assert on.
        let script = b"#!/bin/sh\nexit 7\n";
        let input = sb.write("rc7.sh", script);

        bin().arg(&input).assert().success();
        let packed = sb.expected("rc7.sh.upxz");

        bin().arg(&packed)
            .assert()
            .failure()
            .code(predicate::eq(7));
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}
