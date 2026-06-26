//! End-to-end CLI tests for the flat (runner-base) upxz CLI.
//!
//! Drives the compiled `upxz` binary via `assert_cmd` through every mode the
//! new (mneme#41) CLI exposes:
//!
//! - pack  : `upxz <file>`           -> writes `<file>.upxz`
//! - run   : `upxz <file>.upxz`      -> decompress + exec (propagates exit code)
//! - --bin : `upxz --bin <inner> <a.tar.zst> -- args` -> stream-extract one
//!           entry from a .tar.zst and exec it (no full extraction)
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

    bin()
        .arg("-t")
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
    bin()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("upxz"));
}

// ---------------------------------------------------------------------------
// -c create SFX (Linux only): pack a shell script into a self-extractor and
// run it. We use a shell script rather than a native binary so the test is
// portable across architectures inside the Linux container the SFX feature
// targets. The stub relies on memfd_create + fexecve; a `#!/bin/sh` script
// is a perfectly valid "binary" for the kernel to exec from a memfd.
// ---------------------------------------------------------------------------

#[test]
fn create_sfx_runs_packed_script_linux() {
    #[cfg(target_os = "linux")]
    {
        let sb = Sandbox::new("sfx-script");
        let script = b"#!/bin/sh\necho sfx-ran; exit 0\n";
        let input = sb.write("hello.sh", script);
        let packed = sb.expected("hello.packed");

        bin().arg("-c").arg(&input).arg("-o").arg(&packed).assert().success();

        // The SFX must be executable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&packed).unwrap().permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "SFX output is not executable (mode={:o})",
                mode
            );
        }

        // Running it must exec the restored script and print its output.
        let out = std::process::Command::new(&packed)
            .output()
            .expect("run SFX");
        assert!(
            out.status.success(),
            "SFX exited {:?}: stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "sfx-ran");
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SFX is Linux-only; nothing to test on other targets.
    }
}

#[test]
fn create_sfx_propagates_exit_code_linux() {
    #[cfg(target_os = "linux")]
    {
        let sb = Sandbox::new("sfx-rc");
        let script = b"#!/bin/sh\nexit 7\n";
        let input = sb.write("rc7.sh", script);
        let packed = sb.expected("rc7.packed");

        bin().arg("-c").arg(&input).arg("-o").arg(&packed).assert().success();
        std::process::Command::new(&packed)
            .status()
            .expect("run SFX")
            .code()
            .expect("exit code");
        let code = std::process::Command::new(&packed)
            .status()
            .expect("run SFX 2")
            .code()
            .unwrap_or(-1);
        assert_eq!(code, 7, "SFX must propagate the inner program's exit code");
    }
    #[cfg(not(target_os = "linux"))]
    {}
}

// ---------------------------------------------------------------------------
// -c create SFX (macOS only): the two-segment self-extractor
//   `[ upxz-loader ][ .upxz ][ trailer ]`. The loader Mach-O IS the packed
//   file's header (codesigned); `./packed` execs the loader directly, which
//   decompresses the app segment and execs the original. The appended app
//   bytes break `codesign --verify --strict` but exec is unaffected (AMFI
//   accepts the loader's cdhash). These tests pack a shell script as the app
//   so they are architecture-independent inside the macOS host.
// ---------------------------------------------------------------------------

#[test]
fn create_sfx_runs_packed_script_macos() {
    #[cfg(target_os = "macos")]
    {
        let sb = Sandbox::new("sfx-mac-script");
        let script = b"#!/bin/sh\necho sfx-mac-ran; exit 0\n";
        let input = sb.write("hello.sh", script);
        let packed = sb.expected("hello.packed");

        bin().arg("-c").arg(&input).arg("-o").arg(&packed).assert().success();

        // The SFX must be executable. Its Mach-O header IS the loader
        // (codesigned), so `./packed` execs the loader directly.
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&packed).unwrap().permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "macOS SFX output is not executable (mode={:o})",
                mode
            );
        }

        let out = std::process::Command::new(&packed)
            .output()
            .expect("run macOS SFX");
        assert!(
            out.status.success(),
            "macOS SFX exited {:?}: stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "sfx-mac-ran");
    }
    #[cfg(not(target_os = "macos"))]
    {
        // macOS SFX is macOS-only; nothing to test on other targets.
    }
}

#[test]
fn create_sfx_propagates_exit_code_macos() {
    #[cfg(target_os = "macos")]
    {
        let sb = Sandbox::new("sfx-mac-rc");
        let script = b"#!/bin/sh\nexit 7\n";
        let input = sb.write("rc7.sh", script);
        let packed = sb.expected("rc7.packed");

        bin().arg("-c").arg(&input).arg("-o").arg(&packed).assert().success();
        let code = std::process::Command::new(&packed)
            .status()
            .expect("run macOS SFX")
            .code()
            .unwrap_or(-1);
        assert_eq!(
            code, 7,
            "macOS SFX must propagate the inner program's exit code"
        );
    }
    #[cfg(not(target_os = "macos"))]
    {}
}

#[test]
fn create_sfx_forwards_argv_macos() {
    #[cfg(target_os = "macos")]
    {
        let sb = Sandbox::new("sfx-mac-args");
        // Script echoes its own argv so we can verify forwarding verbatim,
        // including hyphen-leading args.
        let script = b"#!/bin/sh\nprintf 'argc=%d argv=%s\\n' \"$#\" \"$*\"\n";
        let input = sb.write("args.sh", script);
        let packed = sb.expected("args.packed");

        bin().arg("-c").arg(&input).arg("-o").arg(&packed).assert().success();
        let out = std::process::Command::new(&packed)
            .args(["-a", "--long", "val", "quoted arg"])
            .output()
            .expect("run macOS SFX");
        assert!(
            out.status.success(),
            "macOS SFX exited {:?}: stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let line = String::from_utf8_lossy(&out.stdout);
        assert!(
            line.contains("argc=4"),
            "expected 4 forwarded args, got: {line}"
        );
        assert!(
            line.contains("-a --long val quoted arg"),
            "argv not forwarded verbatim: {line}"
        );
    }
    #[cfg(not(target_os = "macos"))]
    {}
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

        bin().arg(&packed).assert().failure().code(predicate::eq(7));
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

/// `upxz <file>.upxz -- a b -c` must forward every arg after `--` to the
/// restored binary, including hyphen-leading ones. This is a regression test
/// for a CLI bug where a second positional (`-c`'s output) swallowed the first
/// trailing arg; the SFX output is now `-o`/`--out`, leaving `ARGS` as the
/// only positional after `FILE`.
#[test]
fn run_forwards_trailing_args_after_dash_dash() {
    let sb = Sandbox::new("run-args");
    #[cfg(unix)]
    {
        // Echo all args so we can assert they arrived verbatim.
        let script = b"#!/bin/sh\nprintf 'argc=%d|%s\\n' \"$#\" \"$*\"\n";
        let input = sb.write("echo.sh", script);
        bin().arg(&input).assert().success();
        let packed = sb.expected("echo.sh.upxz");

        let out = bin()
            .arg(&packed)
            .arg("--")
            .args(["first", "-flag", "with space", "--long=1"])
            .assert()
            .success();
        let s = String::from_utf8_lossy(&out.get_output().stdout);
        assert!(
            s.contains("argc=4|"),
            "expected 4 forwarded args, got: {s}"
        );
        assert!(
            s.contains("first -flag with space --long=1"),
            "args not forwarded verbatim: {s}"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

// ---------------------------------------------------------------------------
// --bin : run a single entry out of a .tar.zst without full extraction.
// ---------------------------------------------------------------------------

/// Build a `.tar.zst` at `dst` from the files under `src_dir`. Returns early
/// with a sentinel error message if either `tar` or `zstd` is missing on PATH
/// (the calling test then no-ops rather than failing — the test environment is
/// not guaranteed to ship them).
#[cfg(unix)]
fn build_tar_zst(src_dir: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
    use std::process::Command;
    // tar -cf tmp.tar -C src_dir .
    let tmp_tar = dst.with_extension("tar");
    let st = Command::new("tar")
        .arg("-cf")
        .arg(&tmp_tar)
        .arg("-C")
        .arg(src_dir)
        .arg(".")
        .status()?;
    if !st.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("tar exited {st:?}; is tar on PATH?"),
        ));
    }
    // zstd -19 -f tmp.tar -o dst
    let st = Command::new("zstd")
        .args(["-19", "-f"])
        .arg(&tmp_tar)
        .arg("-o")
        .arg(dst)
        .status()?;
    if !st.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("zstd exited {st:?}; is zstd on PATH?"),
        ));
    }
    let _ = fs::remove_file(&tmp_tar);
    Ok(())
}

/// `upxz --bin bin/hello a.tar.zst -- arg1` runs only the `bin/hello` entry,
/// forwards the trailing args, and propagates the inner exit code. The decoy
/// files in the archive are never extracted.
#[test]
fn bin_runs_inner_entry_and_forwards_argv() {
    let sb = Sandbox::new("bin-run");
    #[cfg(unix)]
    {
        // Build a small archive with:
        //   bin/hello  -> the script we want to run (exits 42, prints argv)
        //   extra/x    -> a decoy that must NOT be extracted
        let src = sb.dir.join("src");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::create_dir_all(src.join("extra")).unwrap();
        let script = b"#!/bin/sh\nprintf 'argc=%d|%s\\n' \"$#\" \"$*\"; exit 42\n";
        fs::write(src.join("bin").join("hello"), script).unwrap();
        fs::write(src.join("extra").join("decoy"), b"decoy").unwrap();

        let archive = sb.dir.join("a.tar.zst");
        if build_tar_zst(&src, &archive).is_err() {
            // No tar/zstd on PATH in this environment — skip, not fail.
            eprintln!("skipping bin_runs_inner_entry_and_forwards_argv: tar/zstd unavailable");
            return;
        }
        assert!(archive.is_file(), "archive should exist");

        // Run only the inner entry, with trailing args.
        let out = bin()
            .arg("--bin")
            .arg("bin/hello")
            .arg(&archive)
            .arg("--")
            .args(["one", "two", "-x"])
            .assert()
            .failure()
            .code(predicate::eq(42));
        let s = String::from_utf8_lossy(&out.get_output().stdout);
        assert!(
            s.contains("argc=3|"),
            "expected 3 forwarded args, got: {s}"
        );
        assert!(
            s.contains("one two -x"),
            "argv not forwarded verbatim: {s}"
        );

        // The decoy must not have been written next to the archive (we only
        // materialize the matched inner entry, into a temp / memfd).
        assert!(
            !sb.dir.join("extra").exists(),
            "decoy dir was created — --bin should not extract unrelated entries"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

/// `--bin ./bin/hello` (leading `./`) must match an entry stored as `bin/hello`.
#[test]
fn bin_matches_entry_with_leading_dot_slash() {
    let sb = Sandbox::new("bin-dot");
    #[cfg(unix)]
    {
        let src = sb.dir.join("src");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::write(
            src.join("bin").join("hi"),
            b"#!/bin/sh\necho ran-inner\n",
        )
        .unwrap();
        let archive = sb.dir.join("b.tar.zst");
        if build_tar_zst(&src, &archive).is_err() {
            eprintln!("skipping bin_matches_entry_with_leading_dot_slash: tar/zstd unavailable");
            return;
        }

        bin()
            .arg("--bin")
            .arg("./bin/hi")
            .arg(&archive)
            .assert()
            .success()
            .stdout(predicate::str::contains("ran-inner"));
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

/// `--bin` on a path that is not in the archive must error, not silently exit 0.
#[test]
fn bin_missing_entry_errors() {
    let sb = Sandbox::new("bin-missing");
    #[cfg(unix)]
    {
        let src = sb.dir.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("real"), b"#!/bin/sh\necho x\n").unwrap();
        let archive = sb.dir.join("c.tar.zst");
        if build_tar_zst(&src, &archive).is_err() {
            eprintln!("skipping bin_missing_entry_errors: tar/zstd unavailable");
            return;
        }

        bin()
            .arg("--bin")
            .arg("does/not/exist")
            .arg(&archive)
            .assert()
            .failure()
            .stderr(predicate::str::contains("did not find entry"));
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}
