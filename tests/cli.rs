//! End-to-end CLI tests for the upx-style upxz CLI.
//!
//! Drives the compiled `upxz` binary via `assert_cmd` through every mode:
//!
//! - pack    : `upxz <file>`              -> writes `<file>.upxz` (a
//!   **self-extractor** with chmod +x; `./<file>.upxz` runs the original)
//! - refuse  : `upxz <file>.upxz`         -> refused (already packed; run the
//!   .upxz directly, or use `-d` to restore the original)
//! - -c      : `upxz -c <file> -o <out>`  -> self-extractor at an explicit path
//! - -d      : `upxz -d <file>.upxz`      -> unpack (executable bit restored
//!   when the original was an executable)
//! - -l / -t : list / test on the SFX (locate the embedded UPXZ container)
//! - --bin   : `upxz --bin <inner> <a.tar.zst> -- args` -> stream-extract one
//!   entry from a .tar.zst and exec it (no full extraction)
//!
//! The self-extractor embeds a UPXZ container (`UPXZ\x01 + codec + 0 + 0`,
//! then `name_len + name + compressed payload`) after a tiny platform
//! stub/loader and a short trailer. The container magic is `UPXZ\x01\x00\x00\x00`
//! (8 bytes) but lives at `stub_size` — not at offset 0.

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

/// Assert that `path` is a self-extracting executable: executable bits set, and
/// it embeds a UPXZ container somewhere past the leading stub/loader. Used for
/// the default-pack output, which is a self-extractor (not a bare container).
fn assert_is_self_extractor(path: &PathBuf) {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "self-extractor {} is not executable (mode={:o})",
            path.display(),
            mode
        );
    }
    // The SFX embeds the UPXZ container past the platform stub/loader, so the
    // UPXZ prefix appears at offset > 0.
    let prefix = b"UPXZ\x01";
    let off = bytes
        .windows(prefix.len())
        .position(|w| w == prefix)
        .unwrap_or_else(|| panic!("no embedded UPXZ container in {}", path.display()));
    assert!(
        off > 0,
        "UPXZ prefix at offset 0 means this is a bare container, not an SFX: {}",
        path.display()
    );
    // The embedded container must also be a valid codec id (0 = zstd, 1 = gzip).
    assert!(
        bytes[off + 5] == 0 || bytes[off + 5] == 1,
        "embedded codec byte in {} is {} (expected zstd/gzip)",
        path.display(),
        bytes[off + 5]
    );
    // Reserved bytes at offset 6, 7 must be 0 (matches the container contract).
    assert_eq!(bytes[off + 6], 0, "reserved byte 6 in {}", path.display());
    assert_eq!(bytes[off + 7], 0, "reserved byte 7 in {}", path.display());
}

/// Find the embedded UPXZ container offset inside an SFX and assert its codec
/// id. Used by the gzip pack tests, which need to verify the embedded
/// container's codec byte (the SFX's offset 5 is inside the platform stub and
/// is not the container codec byte).
fn assert_embedded_codec_is(path: &PathBuf, codec_id: u8) {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let prefix = b"UPXZ\x01";
    let off = bytes
        .windows(prefix.len())
        .position(|w| w == prefix)
        .unwrap_or_else(|| panic!("no embedded UPXZ container in {}", path.display()));
    assert_eq!(
        bytes[off + 5],
        codec_id,
        "embedded codec byte in {} must be {}",
        path.display(),
        codec_id
    );
    assert_eq!(bytes[off + 6], 0, "reserved byte 6 in {}", path.display());
    assert_eq!(bytes[off + 7], 0, "reserved byte 7 in {}", path.display());
}

// ---------------------------------------------------------------------------
// pack
// ---------------------------------------------------------------------------

#[test]
fn pack_plain_file_writes_self_extractor() {
    // The default pack now produces a self-extractor (not a bare container).
    // It must be executable and embed a UPXZ container past the stub/loader.
    let sb = Sandbox::new("pack-sfx");
    let input = sb.write("hello.txt", b"hello upxz\n".repeat(64).as_slice());
    let expected = sb.expected("hello.txt.upxz");

    bin().arg(&input).assert().success();

    assert!(expected.is_file(), "{} should exist", expected.display());
    assert_is_self_extractor(&expected);
}

#[test]
fn pack_refuses_already_packed_file() {
    // Re-feeding an already-packed upxz artifact (bare container or self-
    // extractor) to `upxz` must be refused — the new model has no "run mode"
    // inside upxz; the user runs `./<file>.upxz` directly instead. The
    // refuse check fires from the default dispatch (classify -> Packed ->
    // bail), not from check_packable_input (which now lives inside pack_sfx).
    let sb = Sandbox::new("refuse-packed");
    let input = sb.write("in.bin", b"some payload bytes".repeat(32).as_slice());

    bin().arg(&input).assert().success();
    let packed = sb.expected("in.bin.upxz");
    assert!(packed.is_file());

    // Re-feed the SFX to upxz: must fail with the "already packed" refuse.
    bin()
        .arg(&packed)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already a packed upxz"));
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
    // The default pack now emits a self-extractor (not a bare container), so
    // the UPXZ magic is embedded past the stub/loader, not at offset 0.
    assert_is_self_extractor(&packed);

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

    // The default pack produces a self-extractor; `-l` reports it as such and
    // shows the embedded container's stats, not the whole file size.
    bin().arg("-l").arg(&packed).assert().success().stdout(
        predicate::str::contains("kind\tupxz self-extractor")
            .and(predicate::str::contains("codec\tzstd"))
            .and(predicate::str::contains("name\tnote.txt"))
            .and(predicate::str::contains("packed\t"))
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
    // Default pack now produces a self-extractor, not a bare container.
    assert_is_self_extractor(&packed);

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

        bin()
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(&packed)
            .assert()
            .success();

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

        bin()
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(&packed)
            .assert()
            .success();
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

        bin()
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(&packed)
            .assert()
            .success();

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

        bin()
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(&packed)
            .assert()
            .success();
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

        bin()
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(&packed)
            .assert()
            .success();
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
fn sfx_runs_packed_script_and_propagates_exit_zero() {
    // The new model has no `upxz <packed>` runner: the SFX runs directly.
    // Pack to SFX, then exec the SFX (./<packed>.upxz is the new "run").
    let sb = Sandbox::new("sfx-run-zero");
    #[cfg(unix)]
    {
        let script = b"#!/bin/sh\nexit 0\n";
        let input = sb.write("ok.sh", script);

        bin().arg(&input).assert().success();
        let packed = sb.expected("ok.sh.upxz");
        assert_is_self_extractor(&packed);

        // Running the SFX must exec the restored script and propagate 0.
        let status = std::process::Command::new(&packed)
            .status()
            .expect("run SFX");
        assert!(status.success(), "SFX exited {status:?}");
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

#[test]
fn sfx_propagates_nonzero_exit_code() {
    let sb = Sandbox::new("sfx-run-nonzero");
    #[cfg(unix)]
    {
        // `exit 7` — a distinctive code we can assert on.
        let script = b"#!/bin/sh\nexit 7\n";
        let input = sb.write("rc7.sh", script);

        bin().arg(&input).assert().success();
        let packed = sb.expected("rc7.sh.upxz");

        let status = std::process::Command::new(&packed)
            .status()
            .expect("run SFX");
        assert_eq!(status.code(), Some(7), "SFX must propagate inner exit 7");
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

/// `upxz <file>.upxz -- a b -c` (old) → now: run the SFX directly, passing
/// args. The SFX forwards every trailing arg verbatim to the inner program,
/// including hyphen-leading ones. This is the runtime contract the SFX
/// loader (Linux memfd+fexecve, macOS codesign+execv) must honor.
#[test]
fn sfx_forwards_trailing_args_after_dash_dash() {
    let sb = Sandbox::new("sfx-run-args");
    #[cfg(unix)]
    {
        let script = b"#!/bin/sh\nprintf 'argc=%d|%s\\n' \"$#\" \"$*\"\n";
        let input = sb.write("echo.sh", script);
        bin().arg(&input).assert().success();
        let packed = sb.expected("echo.sh.upxz");

        let out = std::process::Command::new(&packed)
            .args(["first", "-flag", "with space", "--long=1"])
            .output()
            .expect("run SFX");
        assert!(
            out.status.success(),
            "SFX exited {:?}: {:?}",
            out.status,
            out
        );
        let s = String::from_utf8_lossy(&out.stdout);
        assert!(s.contains("argc=4|"), "expected 4 forwarded args, got: {s}");
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
        return Err(std::io::Error::other(format!(
            "tar exited {st:?}; is tar on PATH?"
        )));
    }
    // zstd -19 -f tmp.tar -o dst
    let st = Command::new("zstd")
        .args(["-19", "-f"])
        .arg(&tmp_tar)
        .arg("-o")
        .arg(dst)
        .status()?;
    if !st.success() {
        return Err(std::io::Error::other(format!(
            "zstd exited {st:?}; is zstd on PATH?"
        )));
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
        assert!(s.contains("argc=3|"), "expected 3 forwarded args, got: {s}");
        assert!(s.contains("one two -x"), "argv not forwarded verbatim: {s}");

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
        fs::write(src.join("bin").join("hi"), b"#!/bin/sh\necho ran-inner\n").unwrap();
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

// ---------------------------------------------------------------------------
// codec-agnostic: --gz selects gzip (codec id 1 in the magic). The whole
// read path (run / unpack / list / test) must dispatch by the codec byte, and
// a zstd container (codec id 0) must still round-trip (backward compat).
// ---------------------------------------------------------------------------

// codec-agnostic: --gz selects gzip (codec id 1 in the embedded container's
// magic). The default pack always produces a self-extractor now, so the gzip
// codec byte lives at the *embedded* offset — see `assert_embedded_codec_is`
// above. The whole read path (`-l`/`-t`/`-d`) dispatches on that byte.
//

#[test]
fn pack_gz_writes_gzip_magic() {
    // The macOS SFX loader is zstd-only for size, so a gzip SFX is refused on
    // macOS (`gzip_sfx_rejected_on_macos` covers that path). Linux and Windows
    // support gzip SFXes.
    #[cfg(not(target_os = "macos"))]
    {
        let sb = Sandbox::new("pack-gz-magic");
        let input = sb.write("hello.txt", b"hello upxz gzip\n".repeat(64).as_slice());
        let expected = sb.expected("hello.txt.upxz");

        bin().arg("--gz").arg(&input).assert().success();
        assert!(expected.is_file());
        // The SFX embeds the gzip container; the codec byte is at the
        // embedded offset, not at offset 0 (which is inside the platform stub).
        assert_embedded_codec_is(&expected, 1);
    }
}

#[test]
fn pack_gz_unpack_roundtrips_bytes() {
    #[cfg(not(target_os = "macos"))]
    {
        let sb = Sandbox::new("pack-gz-rt");
        let body = b"the gzip round trip\n".repeat(50);
        let input = sb.write("payload.bin", &body);

        bin().arg("--gz").arg(&input).assert().success();
        let packed = sb.expected("payload.bin.upxz");
        assert_embedded_codec_is(&packed, 1);

        // Unpack must restore byte-for-byte through the gzip codec path. The
        // restore goes through classify -> locate the embedded container ->
        // decompress, so `-d` on the SFX works the same as on a bare container.
        bin().arg("-d").arg(&packed).arg("-f").assert().success();
        assert_eq!(fs::read(sb.expected("payload.bin")).unwrap(), body);
    }
}

#[test]
fn pack_gz_test_reports_gzip() {
    #[cfg(not(target_os = "macos"))]
    {
        let sb = Sandbox::new("pack-gz-test");
        let input = sb.write("data.bin", b"abc".repeat(100).as_slice());
        bin().arg("--gz").arg(&input).assert().success();
        let packed = sb.expected("data.bin.upxz");

        bin().arg("-t").arg(&packed).assert().success().stdout(
            predicate::str::contains("ok\t")
                .and(predicate::str::contains("gzip round-trip ok"))
                .and(predicate::str::contains("data.bin")),
        );
    }
}

#[test]
fn pack_gz_list_shows_gzip_codec() {
    #[cfg(not(target_os = "macos"))]
    {
        let sb = Sandbox::new("pack-gz-list");
        let input = sb.write("note.txt", b"hello world\n".repeat(16).as_slice());
        bin().arg("--gz").arg(&input).assert().success();
        let packed = sb.expected("note.txt.upxz");

        // `-l` reports the embedded container's codec ("gzip"), not a bare
        // container's "magic\tUPXZ" line.
        bin()
            .arg("-l")
            .arg(&packed)
            .assert()
            .success()
            .stdout(predicate::str::contains("codec\tgzip"));
    }
}

#[test]
fn pack_gz_runs_packed_script() {
    // The SFX runtime is the new "run" — the SFX must exec the gzip-decoded
    // original (Linux/Windows SFX stub supports gzip; macOS refuses gzip SFXes).
    #[cfg(not(target_os = "macos"))]
    {
        let sb = Sandbox::new("pack-gz-run");
        #[cfg(unix)]
        {
            let script = b"#!/bin/sh\necho gz-ran; exit 0\n";
            let input = sb.write("hello.sh", script);

            bin().arg("--gz").arg(&input).assert().success();
            let packed = sb.expected("hello.sh.upxz");
            assert_embedded_codec_is(&packed, 1);

            // Run the SFX directly (the new "run" path).
            let out = std::process::Command::new(&packed)
                .output()
                .expect("run gzip SFX");
            assert!(
                out.status.success(),
                "gzip SFX exited {:?}: stderr={}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "gz-ran");
        }
        #[cfg(not(unix))]
        {
            let _ = sb;
        }
    }
}

#[test]
fn backcompat_zstd_container_still_works_after_codec_aware_build() {
    // A default-pack output (no --gz) must still use codec id 0 (zstd) in the
    // *embedded* container. This is the regression guard for "did we break
    // the v0.1/v0.2 format?". The UPXZ magic is no longer at offset 0 — the
    // default pack emits a self-extractor, so the codec byte lives at the
    // embedded offset.
    let sb = Sandbox::new("backcompat");
    let body = b"backward compat zstd bytes\n".repeat(40);
    let input = sb.write("old.bin", &body);

    bin().arg(&input).assert().success(); // no --gz => zstd
    let packed = sb.expected("old.bin.upxz");
    assert_embedded_codec_is(&packed, 0); // 0 = zstd

    bin().arg("-d").arg(&packed).arg("-f").assert().success();
    assert_eq!(fs::read(sb.expected("old.bin")).unwrap(), body);
}

// ===========================================================================
// Regression: read-only ops (-l/-t/-d) must work WITHOUT --create-sfx.
//
// During the SFX refactors a clap-derive structural change briefly made
// --create-sfx a required argument, which silently broke `upxz -d`, `-l`,
// and `-t` standalone (mneme issue #14). That was a stale-binary false alarm
// — the dispatch has always treated these as independent actions — but the
// regression is cheap to guard against forever: each read op must succeed on
// a plain container with no -c present.
// ===========================================================================

#[test]
fn read_ops_do_not_require_create_sfx() {
    let sb = Sandbox::new("no-c");
    let input = sb.write("in.bin", b"payload bytes here".repeat(20).as_slice());
    bin().arg(&input).assert().success();
    let packed = sb.expected("in.bin.upxz");

    // -l and -t are read-only and must succeed with NO -c anywhere.
    bin().arg("-l").arg(&packed).assert().success();
    bin().arg("-t").arg(&packed).assert().success();
    // -d restores; -f because the original still sits next to the container.
    bin().arg("-d").arg(&packed).arg("-f").assert().success();
}

// ===========================================================================
// CLI argument validation: mutually-required / required-when-set flags and
// usage errors. These exercise clap's structural invariants, not compression.
// ===========================================================================

/// `-c` without `-o` has nowhere to write the SFX → upxz errors (exit != 0)
/// before any platform dispatch.
#[test]
fn create_sfx_without_out_errors() {
    let sb = Sandbox::new("c-no-o");
    let input = sb.write("in.bin", b"data".repeat(32).as_slice());
    bin().arg("-c").arg(&input).assert().failure();
}

/// `-o` is declared `requires = "create_sfx"`, so `-o` without `-c` is a clap
/// usage error (non-zero exit), not a pack.
#[test]
fn out_without_create_sfx_is_rejected() {
    let sb = Sandbox::new("o-no-c");
    let input = sb.write("in.bin", b"data".repeat(32).as_slice());
    bin()
        .arg("-o")
        .arg(sb.expected("out"))
        .arg(&input)
        .assert()
        .failure();
}

/// No arguments at all → clap prints usage and exits non-zero.
#[test]
fn no_args_prints_usage() {
    bin().assert().failure();
}

/// A non-existent input file must error, not silently succeed.
#[test]
fn missing_input_file_errors() {
    let sb = Sandbox::new("missing");
    bin()
        .arg("-l")
        .arg(sb.expected("does-not-exist.upxz"))
        .assert()
        .failure();
}

/// `-z 0` is below the 1..=19 range → clap value_parser rejects it.
#[test]
fn z_level_zero_is_rejected() {
    let sb = Sandbox::new("z0");
    let input = sb.write("in.bin", b"data".repeat(32).as_slice());
    bin().arg("-z").arg("0").arg(&input).assert().failure();
}

/// `-z 20` is above the 1..=19 range (20..=22 need --ultra; -22 is a trap) →
/// rejected. Guards the "never -22" ceiling.
#[test]
fn z_level_above_nineteen_is_rejected() {
    let sb = Sandbox::new("z20");
    let input = sb.write("in.bin", b"data".repeat(32).as_slice());
    bin().arg("-z").arg("20").arg(&input).assert().failure();
}

/// An empty input file must still pack to a valid self-extractor and round-
/// trip to zero bytes. (zstd happily encodes an empty frame.)
#[test]
fn pack_empty_file_roundtrips() {
    let sb = Sandbox::new("empty");
    let input = sb.write("empty.bin", b"");
    bin().arg(&input).assert().success();
    let packed = sb.expected("empty.bin.upxz");
    assert_is_self_extractor(&packed);

    bin().arg("-d").arg(&packed).arg("-f").assert().success();
    assert_eq!(fs::read(sb.expected("empty.bin")).unwrap(), b"");
}

/// Corrupting a byte inside the compressed payload must make `-t` fail: the
/// zstd content checksum (written by the default Encoder) no longer matches
/// the restored bytes, so decode errors. The body is high-entropy so the
/// payload is large enough that a corruption near its tail lands on real
/// compressed bytes, not past the end of the file.
#[test]
fn test_rejects_corrupted_payload() {
    let sb = Sandbox::new("corrupt");
    // Pseudo-random, low-compressibility body ⇒ payload stays large.
    let body: Vec<u8> = (0..3000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    let input = sb.write("data.bin", &body);
    bin().arg(&input).assert().success();
    let packed = sb.expected("data.bin.upxz");

    let mut bytes = fs::read(&packed).unwrap();
    // Corrupt one byte 32 from the end: definitely inside the payload (which
    // is the whole tail of a container — there is no trailer) and far enough
    // from the 4-byte trailing checksum to mutate compressed data, so the
    // recomputed checksum will not match the stored one.
    let pos = bytes.len() - 32;
    bytes[pos] ^= 0xff;
    fs::write(&packed, &bytes).unwrap();

    bin().arg("-t").arg(&packed).assert().failure();
}

// ===========================================================================
// SFX runtime contracts (unix: linux stub + macos loader share these). The
// self-extractor must behave like the original binary for fd inheritance and
// must fail cleanly — not crash or exec garbage — on a tampered packed file.
// ===========================================================================

/// `./packed` must inherit the parent's stdin and pipe it to the inner program
/// (the loader's execv inherits fds by default). A `cat`-style inner echoes
/// stdin back to stdout verbatim.
#[test]
fn sfx_inherits_stdin_to_inner() {
    let sb = Sandbox::new("sfx-stdin");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::process::Stdio;

        let script = b"#!/bin/sh\ncat\n";
        let input = sb.write("cat.sh", script);
        let packed = sb.expected("cat.packed");
        bin()
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(&packed)
            .assert()
            .success();

        let mut child = std::process::Command::new(&packed)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn SFX");
        {
            let mut stdin = child.stdin.take().expect("piped stdin");
            stdin
                .write_all(b"hello-through-stdin\n")
                .expect("write stdin");
        }
        let out = child.wait_with_output().expect("wait SFX");
        assert!(out.status.success(), "SFX stdin test exited {:#?}", out);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "hello-through-stdin",
            "stdin was not forwarded to the inner program"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

/// The inner program's stdout and stderr must stay on their correct streams
/// through the SFX exec — stdout must NOT contain the stderr text.
#[test]
fn sfx_separates_stdout_and_stderr() {
    let sb = Sandbox::new("sfx-streams");
    #[cfg(unix)]
    {
        use std::process::Stdio;

        let script = b"#!/bin/sh\necho OUT-LINE; echo ERR-LINE 1>&2\n";
        let input = sb.write("io.sh", script);
        let packed = sb.expected("io.packed");
        bin()
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(&packed)
            .assert()
            .success();

        let out = std::process::Command::new(&packed)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run SFX");
        assert!(out.status.success(), "SFX exited {:#?}", out);
        let so = String::from_utf8_lossy(&out.stdout);
        let se = String::from_utf8_lossy(&out.stderr);
        assert!(so.contains("OUT-LINE"), "stdout missing OUT-LINE: {so}");
        assert!(se.contains("ERR-LINE"), "stderr missing ERR-LINE: {se}");
        assert!(
            !so.contains("ERR-LINE"),
            "stderr text leaked into stdout: {so}"
        );
        assert!(
            !se.contains("OUT-LINE"),
            "stdout text leaked into stderr: {se}"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

/// A packed file whose trailer has been truncated is malformed. Running it
/// must fail cleanly with a non-zero exit (loader/stub detects the bad trailer
/// / lengths and aborts) — never exec arbitrary bytes or crash the harness.
#[test]
fn sfx_rejects_truncated_packed_file() {
    let sb = Sandbox::new("sfx-trunc");
    #[cfg(unix)]
    {
        use std::process::Stdio;

        let script = b"#!/bin/sh\necho should-not-run\n";
        let input = sb.write("v.sh", script);
        let packed = sb.expected("v.packed");
        bin()
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(&packed)
            .assert()
            .success();

        // Drop the last 16 bytes: on macOS that removes the whole
        // UPXZEND1+loader_len+app_len trailer; on linux it removes the 8-byte
        // stub_size trailer plus payload tail. Either way the self-extractor
        // can no longer locate its segments.
        let mut bytes = fs::read(&packed).unwrap();
        let cut = bytes.len().saturating_sub(16);
        bytes.truncate(cut);
        let truncated = sb.expected("v-trunc.packed");
        fs::write(&truncated, &bytes).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&truncated, fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Run the tampered SFX. On Linux this can transiently fail with
        // ETXTBSY ("Text file busy", errno 26): execve refuses a file the
        // kernel still sees as open-for-writing right after we wrote it. The
        // window is short (writeback / close propagation on the runner's fs),
        // so we RETRY — the exec eventually succeeds and the truncated SFX
        // then fails cleanly (bad trailer). A fresh-file path alone did NOT
        // eliminate this on ubuntu x86_64 (flaked twice on CI); the retry is
        // the real fix. macOS does not exhibit ETXTBSY here.
        let mut status = None;
        for _ in 0..30 {
            match std::process::Command::new(&truncated)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .status()
            {
                Ok(s) => {
                    status = Some(s);
                    break;
                }
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    continue;
                }
                Err(e) => panic!("run truncated SFX failed (non-ETXTBSY): {e}"),
            }
        }
        let status = status.expect("run truncated SFX: still ETXTBSY after 30 retries");
        assert!(
            !status.success(),
            "truncated SFX must not exit 0 (status={:?})",
            status
        );
    }
    #[cfg(not(unix))]
    {
        let _ = sb;
    }
}

// ===========================================================================
// macOS-only: the no_std loader is zstd-only for size, so a gzip SFX must be
// refused up front with a clear message rather than emit a packed file the
// loader cannot run. (Linux stub + the cross-platform runner both support gzip.)
// ===========================================================================

#[test]
fn gzip_sfx_rejected_on_macos() {
    let sb = Sandbox::new("gz-sfx-mac");
    #[cfg(target_os = "macos")]
    {
        let input = sb.write("in.sh", b"#!/bin/sh\necho x\n");
        bin()
            .arg("--gz")
            .arg("-c")
            .arg(&input)
            .arg("-o")
            .arg(sb.expected("gz.packed"))
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "gzip SFX is not supported on macOS",
            ));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = sb;
    }
}
