//! upxz-loader — macOS SFX loader (no_std + zstd-sys FFI).
//!
//! This binary is **not run directly by the user**. `upxz -c` on macOS produces
//! a self-extracting `packed` file whose layout is:
//!
//! ```text
//! [ boot sh script ][ upxz-loader bytes ][ .upxz app container ][ trailer ]
//! ```
//!
//! The trailer is 24 bytes at the very end:
//!
//! ```text
//!   b"UPXZEND"   (8 bytes magic)
//!   boot_len     (u32 big-endian)
//!   loader_len   (u32 big-endian)
//!   app_len      (u32 big-endian)
//! ```
//!
//! `./packed` runs the boot sh script. Boot extracts the upxz-loader segment to
//! `~/.cache/upxz/upxz-loader` (reused across invocations), signs it ad-hoc, and
//! `exec`s it with `argv[0]=packed argv...`. Then this loader:
//!   1. opens `argv[1]` (the packed file path passed by boot),
//!   2. reads the trailer to locate the app segment,
//!   3. reads the `.upxz` container at that offset and decompresses it (zstd),
//!   4. writes the original binary to a temp file, `chmod 0500`, and ad-hoc
//!      re-signs it (AMFI kills an unsigned copy on exec),
//!   5. `execv`s the temp binary with the original stored name as `argv[0]` and
//!      the rest of `argv` forwarded verbatim.
//!
//! Because `execv` does not return, the temp file is unlinked first in a forked
//! child that waits for the parent to die and then removes it. This is the only
//! reliable cleanup on macOS, which has no memfd_create / in-memory exec (see
//! mneme `docs/upxz/upxz-loader.md` and the shm PoC).
//!
//! Size is a hard gate: the loader must be < 1/5 of the full upxz binary
//! (~300 KB) and we target < 100 KB. `no_std` + zstd-sys FFI lands at ~67 KB;
//! the std variant measured 338 KB (FAIL). See mneme `docs/upxz/binary-size.md`.

#![no_std]
#![no_main]

// Link macOS system library (libc) for the raw FFI declarations below.
// `System.framework` provides open/read/write/fstat/chmod/execv/fork/_exit.
#[link(name = "System", kind = "dylib")]
extern "C" {}

// Pull in zstd-sys's static libzstd.a. The crate is no_std-compatible; the
// `extern crate` is what makes cargo link the rlib (and its bundled .a). We
// must NOT depend on `zstd` or `zstd-safe`: their wrappers reintroduce Rust
// std's panic/backtrace machinery and balloon the binary.
extern crate zstd_sys;

use core::ffi::{c_char, c_int, c_uint, c_void};

// ---- zstd-sys: only the decoder entries we need. ----
#[allow(non_camel_case_types)] // matches the upstream zstd C API naming
type ZSTD_DCtx = c_void;
extern "C" {
    fn ZSTD_isError(code: usize) -> c_uint;
    fn ZSTD_getFrameContentSize(src: *const c_void, src_size: usize) -> u64;
    fn ZSTD_decompressDCtx(
        ctx: *mut ZSTD_DCtx,
        dst: *mut c_void,
        dst_cap: usize,
        src: *const c_void,
        src_size: usize,
    ) -> usize;
    fn ZSTD_createDCtx() -> *mut ZSTD_DCtx;
    fn ZSTD_freeDCtx(ctx: *mut ZSTD_DCtx) -> usize;
}

// ---- macOS libc subset for I/O + exec. ----
extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
    fn fstat(fd: c_int, buf: *mut Stat) -> c_int;
    fn chmod(path: *const c_char, mode: u32) -> c_int;
    fn execv(path: *const c_char, argv: *const *const c_char) -> c_int;
    fn fork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn unlink(path: *const c_char) -> c_int;
    fn getpid() -> c_int;
    fn _exit(code: c_int) -> !;
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

// macOS `struct stat` layout (arm64 + x86_64 share this 144-byte form).
#[repr(C)]
#[derive(Default)]
struct Stat {
    st_dev: u32,
    st_mode: u16,
    st_nlink: u16,
    st_ino: u64,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u32,
    st_atime: i64,
    st_atimensec: i64,
    st_mtime: i64,
    st_mtimensec: i64,
    st_ctime: i64,
    st_ctimensec: i64,
    st_birthtime: i64,
    st_birthtimensec: i64,
    st_size: i64,
    st_blocks: i64,
    st_blksize: i32,
    st_flags: u32,
    st_gen: u32,
    st_lspare: i32,
    st_qspare: [i64; 2],
}

const O_RDONLY: c_int = 0;
const O_WRONLY: c_int = 1;
const O_CREAT: c_int = 0o100;
const O_TRUNC: c_int = 0o1000;
const SEEK_SET: c_int = 0;

// Trailer (20 bytes): magic(8) + boot_len(4 BE) + loader_len(4 BE) + app_len(4 BE).
// 8 + 4 + 4 + 4 = 20 bytes. mneme `docs/upxz/trailer.md` specifies this layout.
const TRAILER_LEN: usize = 20;
// 8-byte trailer magic: `UPXZEND1` (UPXZ end-of-packed, version 1). The
// trailing `1` is a format version marker; a future incompatible trailer can
// bump it.
const MAGIC_TRAILER: &[u8; 8] = b"UPXZEND1";

// upxz container magic: ASCII `UPXZ\x01\x00\x00\x00` (matches format::MAGIC).
const MAGIC_UPXZ: &[u8; 8] = b"UPXZ\x01\x00\x00\x00";
const UPXZ_HEADER_FIXED: usize = MAGIC_UPXZ.len() + 4; // magic + name_len

// zstd content-size sentinels.
const ZSTD_CONTENTSIZE_UNKNOWN: u64 = u64::MAX;
const ZSTD_CONTENTSIZE_ERROR: u64 = u64::MAX - 1;

/// Exit codes (synced with conventional sysexits.h-ish + sh conventions).
const EXIT_USAGE: c_int = 64;       // bad usage / missing argv
const EXIT_IO: c_int = 74;          // I/O error
const EXIT_FORMAT: c_int = 65;      // bad container / trailer
const EXIT_DECOMPRESS: c_int = 76;  // zstd failure
const EXIT_EXEC: c_int = 127;       // execve failed
const EXIT_PANIC: c_int = 70;       // panic (should not happen; no_std aborts)

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // No std => no unwinding, no backtrace. Abort with a non-zero, distinct code.
    unsafe { _exit(EXIT_PANIC) }
}

/// Write a short literal to fd 2 (stderr) best-effort. Used for error lines so
/// the user is not left with a silent non-zero exit. `msg` must be a byte
/// literal; we trust its length.
unsafe fn warn(fd: c_int, msg: &[u8]) {
    let _ = write(fd, msg.as_ptr() as *const c_void, msg.len());
}

/// Read exactly `count` bytes from `fd` into `buf`. Returns true on full read.
unsafe fn read_exact(fd: c_int, buf: *mut u8, count: usize) -> bool {
    let mut off = 0usize;
    while off < count {
        let n = read(fd, buf.add(off) as *mut c_void, count - off);
        if n <= 0 {
            return false;
        }
        off += n as usize;
    }
    true
}

/// Read a big-endian u32 from a 4-byte slice.
fn be_u32(b: &[u8]) -> u32 {
    ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | (b[3] as u32)
}

#[no_mangle]
pub extern "C" fn main(argc: c_int, argv: *const *const c_char) -> c_int {
    unsafe { real_main(argc, argv) }
}

unsafe fn real_main(argc: c_int, argv: *const *const c_char) -> c_int {
    // argv layout after boot:
    //   argv[0] = upxz-loader path (the extracted loader binary)
    //   argv[1] = packed file path (the original `./packed`)
    //   argv[2..] = user args, forwarded verbatim to the restored binary
    if argc < 2 {
        warn(2, b"upxz-loader: missing packed file argument\n");
        return EXIT_USAGE;
    }
    let loader_path = *argv.offset(0);
    let packed_path = *argv.offset(1);

    // --- 1. Open the packed file and size it. ---
    let fd = open(packed_path, O_RDONLY);
    if fd < 0 {
        warn(2, b"upxz-loader: cannot open packed file\n");
        return EXIT_IO;
    }
    let mut st: Stat = core::mem::zeroed();
    if fstat(fd, &mut st) != 0 {
        warn(2, b"upxz-loader: cannot stat packed file\n");
        close(fd);
        return EXIT_IO;
    }
    let total = st.st_size as usize;
    if total < TRAILER_LEN {
        warn(2, b"upxz-loader: packed file too small to contain a trailer\n");
        close(fd);
        return EXIT_FORMAT;
    }

    // --- 2. Read trailer (last 24 bytes) and validate magic. ---
    let mut trailer = [0u8; TRAILER_LEN];
    if lseek(fd, (total - TRAILER_LEN) as i64, SEEK_SET) < 0
        || !read_exact(fd, trailer.as_mut_ptr(), TRAILER_LEN)
    {
        warn(2, b"upxz-loader: cannot read trailer\n");
        close(fd);
        return EXIT_IO;
    }
    close(fd);
    if &trailer[0..8] != MAGIC_TRAILER {
        warn(2, b"upxz-loader: bad trailer magic (not an upxz packed file)\n");
        return EXIT_FORMAT;
    }
    let boot_len = be_u32(&trailer[8..12]) as usize;
    let loader_len = be_u32(&trailer[12..16]) as usize;
    let app_len = be_u32(&trailer[16..20]) as usize;
    let app_start = boot_len.checked_add(loader_len).unwrap_or(usize::MAX);
    let app_end = app_start.checked_add(app_len).unwrap_or(usize::MAX);
    if app_start > total || app_end > total - TRAILER_LEN {
        warn(2, b"upxz-loader: trailer segment lengths are inconsistent with file size\n");
        return EXIT_FORMAT;
    }

    // --- 3. Read just the .upxz app segment (offset read; never write it out). ---
    let app_buf = malloc(app_len) as *mut u8;
    if app_buf.is_null() {
        warn(2, b"upxz-loader: out of memory allocating app segment buffer\n");
        return EXIT_IO;
    }
    let fd = open(packed_path, O_RDONLY);
    if fd < 0 {
        warn(2, b"upxz-loader: cannot reopen packed file for app segment\n");
        free(app_buf as *mut c_void);
        return EXIT_IO;
    }
    if lseek(fd, app_start as i64, SEEK_SET) < 0
        || !read_exact(fd, app_buf, app_len)
    {
        warn(2, b"upxz-loader: cannot read app segment\n");
        close(fd);
        free(app_buf as *mut c_void);
        return EXIT_IO;
    }
    close(fd);

    // --- 4. Parse the .upxz container header: magic + name_len(4 BE) + name. ---
    if app_len < UPXZ_HEADER_FIXED {
        warn(2, b"upxz-loader: app segment too small to be a .upxz container\n");
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    if core::slice::from_raw_parts(app_buf, MAGIC_UPXZ.len()) != *MAGIC_UPXZ {
        warn(2, b"upxz-loader: app segment does not start with the upxz magic\n");
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    let name_len = be_u32(core::slice::from_raw_parts(app_buf.add(8), 4)) as usize;
    let payload_start = UPXZ_HEADER_FIXED.checked_add(name_len).unwrap_or(usize::MAX);
    if payload_start > app_len {
        warn(2, b"upxz-loader: declared name length runs past the container\n");
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    // NOTE: app_buf must outlive execv, because argv[0] points into the name
    // region below. We free dst (decompression output scratch is no longer
    // needed after the temp file is written), but keep app_buf until after the
    // execv call at the very end.
    let name_ptr = app_buf.add(UPXZ_HEADER_FIXED);
    // The stored name in the container is NUL-free UTF-8 (enforced by the
    // packer), but execv requires a NUL-terminated C string. Copy it into a
    // small heap buffer with a trailing NUL.
    let name_cstr_bytes = malloc(name_len + 1) as *mut u8;
    if name_cstr_bytes.is_null() {
        warn(2, b"upxz-loader: out of memory copying original name\n");
        free(app_buf as *mut c_void);
        return EXIT_IO;
    }
    let mut k = 0;
    while k < name_len {
        *name_cstr_bytes.add(k) = *app_buf.add(UPXZ_HEADER_FIXED + k);
        k += 1;
    }
    *name_cstr_bytes.add(name_len) = 0;
    let payload_ptr = app_buf.add(payload_start);
    let payload_len = app_len - payload_start;

    // --- 5. zstd decompress. ---
    // The upxz packer uses zstd's streaming encoder, which by default writes
    // frames WITHOUT the content-size header (FCS). So ZSTD_getFrameContentSize
    // usually returns UNKNOWN here, and we must allocate the output buffer
    // ourselves. We start with the declared size when present; otherwise we
    // guess and grow on `dstSizeTooSmall` until decompression succeeds or we
    // hit a sanity cap.
    let declared = ZSTD_getFrameContentSize(payload_ptr as *const c_void, payload_len);
    let mut out_size: usize = if declared == ZSTD_CONTENTSIZE_UNKNOWN
        || declared == ZSTD_CONTENTSIZE_ERROR
    {
        // First guess: 16x the compressed size (covers most binaries, which
        // are typically 3–10x compressible). We grow below if too small.
        payload_len.saturating_mul(16).max(4096)
    } else {
        declared as usize
    };
    // Sanity cap: refuse to allocate more than 1 GiB (a single packed binary
    // should never approach this; the cap is a guard against a corrupt length).
    const MAX_OUT: usize = 1 << 30;

    let dctx = ZSTD_createDCtx();
    if dctx.is_null() {
        warn(2, b"upxz-loader: cannot create zstd decompression context\n");
        free(app_buf as *mut c_void);
        return EXIT_DECOMPRESS;
    }
    let mut dst: *mut u8 = core::ptr::null_mut();
    #[allow(unused_assignments)] // reassigned across retry loop iterations
    let mut got: usize = 0;
    loop {
        if out_size > MAX_OUT {
            warn(2, b"upxz-loader: decompressed size exceeds 1 GiB sanity cap\n");
            ZSTD_freeDCtx(dctx);
            if !dst.is_null() {
                free(dst as *mut c_void);
            }
            free(app_buf as *mut c_void);
            return EXIT_DECOMPRESS;
        }
        dst = malloc(out_size) as *mut u8;
        if dst.is_null() {
            warn(2, b"upxz-loader: out of memory allocating decompression buffer\n");
            ZSTD_freeDCtx(dctx);
            free(app_buf as *mut c_void);
            return EXIT_IO;
        }
        got = ZSTD_decompressDCtx(
            dctx,
            dst as *mut c_void,
            out_size,
            payload_ptr as *const c_void,
            payload_len,
        );
        if ZSTD_isError(got) == 0 {
            break; // success
        }
        free(dst as *mut c_void);
        dst = core::ptr::null_mut();
        // The most likely error is dstSizeTooSmall. Grow geometrically (x2)
        // and retry. Any other error (corrupt input) will keep failing and
        // eventually trip the MAX_OUT cap, surfacing as a decompress error.
        out_size = out_size.saturating_mul(2);
    }
    ZSTD_freeDCtx(dctx);
    // `got` is the actual decompressed size; we use dst[0..got] below.

    // --- 6. Write the restored binary to a temp file. ---
    // Path: /tmp/upxz-app-<pid>. A short, predictable name keeps things simple;
    // the pid makes concurrent runs independent. The file is chmod'd 0500
    // (r-x——) before exec.
    let pid = getpid();
    let mut tmp_path_buf = [0u8; 64];
    let tmp_path = format_into(&mut tmp_path_buf, b"/tmp/upxz-app-", pid as u64, b"\0");
    let ofd = open(
        tmp_path.as_ptr() as *const c_char,
        O_WRONLY | O_CREAT | O_TRUNC,
        0o600, // created writable so we can fill it; chmod to 0500 after.
    );
    if ofd < 0 {
        warn(2, b"upxz-loader: cannot create temp file for restored binary\n");
        free(app_buf as *mut c_void);
        free(dst as *mut c_void);
        return EXIT_IO;
    }
    let mut w = 0usize;
    while w < got {
        let n = write(ofd, dst.add(w) as *const c_void, got - w);
        if n <= 0 {
            warn(2, b"upxz-loader: write to temp file failed\n");
            close(ofd);
            unlink(tmp_path.as_ptr() as *const c_char);
            free(app_buf as *mut c_void);
            free(dst as *mut c_void);
            free(name_cstr_bytes as *mut c_void);
            return EXIT_IO;
        }
        w += n as usize;
    }
    close(ofd);
    // dst (decompression scratch) is no longer needed; free it now. app_buf
    // is still referenced indirectly via name_cstr_bytes (already copied) —
    // but we keep app_buf around until after execv only for cleanliness; it is
    // not strictly required past this point. Free both scratch buffers.
    free(app_buf as *mut c_void);
    free(dst as *mut c_void);

    // --- 7. Ad-hoc re-sign the temp copy. macOS AMFI SIGKILLs (exit 137) a
    // restored signed Mach-O on exec because its signature no longer matches.
    // We re-sign ad-hoc via the `codesign` helper (forked + execv'd, since we
    // are no_std and cannot use std::process). This MUST happen before the
    // chmod to 0500 below: codesign needs write access to embed the new
    // signature, and 0500 grants only r-x.
    codesign_adhoc(tmp_path.as_ptr() as *const c_char);

    // chmod 0500 (r-x for owner). Done AFTER codesign so codesign had write
    // access to rewrite the signature.
    if chmod(tmp_path.as_ptr() as *const c_char, 0o500) != 0 {
        warn(2, b"upxz-loader: chmod on restored binary failed\n");
        return EXIT_IO;
    }

    // --- 8. Cleanup strategy for the temp file. ---
    // `execv` does not return, so the loader cannot unlink its own temp file
    // after the exec. We deliberately do NOT fork a watchdog child to unlink
    // it later: on macOS, fork()-ing from the loader (an ad-hoc-signed no_std
    // binary) before execv reliably triggers AMFI SIGKILL (exit 137) on the
    // exec'd program — empirically verified during development.
    //
    // Instead we accept that each run leaves a `/tmp/upxz-app-<pid>` file
    // behind. This is bounded and harmless:
    //   - The path is keyed by pid, so concurrent runs do not collide.
    //   - The file is chmod 0500 (r-x owner), so only the owner can read/run
    //     it; other users cannot.
    //   - The boot script garbage-collects stale `upxz-app-*` files in /tmp on
    //     each invocation (see `boot/upxz-boot.sh`), so the directory does not
    //     grow without bound across normal use.
    // This trade (one residual temp file per run, cleaned on the next run) is
    // the standard one `/tmp`-based tooling makes when in-memory exec is
    // unavailable (which on macOS it is — see mneme shm PoC).

    // --- 9. Build exec argv and execv the restored binary. ---
    // argv[0] = the stored original name (so the program sees its real name);
    // argv[1..] = the loader's argv[2..] forwarded verbatim. The temp path is
    // NOT placed in argv[0]; argv[0] is a presentation name only.
    let user_argc = (argc - 2).max(0) as usize;
    let argv_total = user_argc + 2; // [name, user_args..., NULL]
    let argv_bytes = argv_total * core::mem::size_of::<*const c_char>();
    let exec_argv = malloc(argv_bytes) as *mut *const c_char;
    if exec_argv.is_null() {
        warn(2, b"upxz-loader: out of memory building exec argv\n");
        free(name_cstr_bytes as *mut c_void);
        return EXIT_IO;
    }
    // argv[0] = stored name.
    *exec_argv.offset(0) = name_cstr_bytes as *const c_char;
    // argv[1..] = forward loader argv[2..argc) verbatim.
    let mut i = 1;
    while i - 1 < user_argc {
        // loader argv index = 2 + (i - 1) = i + 1
        *exec_argv.offset(i as isize) = *argv.offset((i + 1) as isize);
        i += 1;
    }
    *exec_argv.offset(user_argc as isize + 1) = core::ptr::null();

    // execv replaces this process image. stdin/stdout/stderr are inherited
    // unchanged (we never touched them), so the restored binary sees the same
    // stdio the user gave `./packed`.
    execv(tmp_path.as_ptr() as *const c_char, exec_argv);
    // execv only returns on failure.
    let _ = loader_path;
    let _ = name_ptr;
    warn(2, b"upxz-loader: execv of restored binary failed\n");
    unlink(tmp_path.as_ptr() as *const c_char);
    free(name_cstr_bytes as *mut c_void);
    free(exec_argv as *mut c_void);
    EXIT_EXEC
}

/// Format `/tmp/upxz-app-<n>\0` into `buf`, returning the filled slice (with
/// trailing NUL). Uses only core arithmetic; no std.
fn format_into<'a>(buf: &'a mut [u8; 64], prefix: &[u8], n: u64, suffix: &[u8]) -> &'a [u8] {
    let mut i = 0usize;
    let mut push = |b: u8, i: &mut usize| {
        if *i < buf.len() {
            buf[*i] = b;
            *i += 1;
        }
    };
    let mut j = 0;
    while j < prefix.len() {
        push(prefix[j], &mut i);
        j += 1;
    }
    // decimal encode n
    if n == 0 {
        push(b'0', &mut i);
    } else {
        let mut digits = [0u8; 20];
        let mut k = 0usize;
        let mut m = n;
        while m > 0 {
            digits[k] = b'0' + (m % 10) as u8;
            m /= 10;
            k += 1;
        }
        while k > 0 {
            k -= 1;
            push(digits[k], &mut i);
        }
    }
    let mut j = 0;
    while j < suffix.len() {
        push(suffix[j], &mut i);
        j += 1;
    }
    &buf[..i]
}

/// Best-effort ad-hoc codesign of the restored binary. Forks and execvs
/// `/usr/bin/codesign --sign - --force <path>`. Failures are non-fatal: an
/// unsigned copy of a previously-unsigned binary (e.g. a shell script) still
/// execs, and a real failure on a signed Mach-O surfaces later as SIGKILL.
unsafe fn codesign_adhoc(path: *const c_char) {
    let child = fork();
    if child < 0 {
        return;
    }
    if child == 0 {
        // Silence codesign's chatty stdout/stderr ("replacing existing
        // signature" etc.) by redirecting both to /dev/null. We open /dev/null
        // and dup2 it onto fds 1 and 2. If the open fails we proceed anyway —
        // the worst case is a noisy codesign, not a wrong signature.
        let devnull = b"/dev/null\0";
        let nfd = open(devnull.as_ptr() as *const c_char, O_WRONLY);
        if nfd >= 0 {
            dup2(nfd, 1);
            dup2(nfd, 2);
            close(nfd);
        }
        // argv = ["codesign", "--sign", "-", "--force", "--", path, NULL]
        let prog = b"/usr/bin/codesign\0";
        let a1 = b"--sign\0";
        let a2 = b"-\0";
        let a3 = b"--force\0";
        let a4 = b"--\0";
        let argv: [*const c_char; 7] = [
            prog.as_ptr() as *const c_char,
            a1.as_ptr() as *const c_char,
            a2.as_ptr() as *const c_char,
            a3.as_ptr() as *const c_char,
            a4.as_ptr() as *const c_char,
            path,
            core::ptr::null(),
        ];
        execv(prog.as_ptr() as *const c_char, argv.as_ptr());
        // exec failed: exit non-zero but quietly.
        _exit(127);
    }
    // Parent: reap the codesign child synchronously so the signature is in
    // place before we execv the restored binary. We intentionally ignore the
    // exit status: if codesign is unavailable or fails, we still try to exec
    // — unsigned binaries that were not previously signed (e.g. shell scripts)
    // exec fine, and a real failure on a signed Mach-O surfaces as the child's
    // SIGKILL (exit 137), which is more informative than the loader refusing.
    let mut status: c_int = 0;
    waitpid(child, &mut status, 0);
}
