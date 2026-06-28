//! upxz-loader — macOS SFX loader (no_std + zstd-sys FFI), two-segment design.
//!
//! In the two-segment macOS SFX, the loader binary **is the packed file's
//! Mach-O header**: `upxz -c` produces
//!
//! ```text
//! [ upxz-loader Mach-O (codesigned) ][ .upxz app container ][ trailer ]
//! ```
//!
//! Running `./packed` executes the loader directly (the kernel sees a Mach-O
//! at offset 0). The trailing app bytes appended after the loader do not stop
//! exec: the kernel reads the `mach_header` to find load commands, and AMFI
//! accepts the loader's cdhash. `codesign --verify` strict reports a failure
//! (the appended bytes perturb the file), but exec is unaffected — verified
//! empirically. This is the same trade-off any appended-payload SFX makes on
//! macOS.
//!
//! The trailer is 16 bytes at the very end:
//!
//! ```text
//!   b"UPXZEND1"  (8 bytes magic)
//!   loader_len   (u32 big-endian)   <- length of the loader Mach-O segment
//!   app_len      (u32 big-endian)   <- length of the .upxz container segment
//! ```
//!
//! Then this loader:
//!   1. resolves its own executable path via `_NSGetExecutablePath` (the loader
//!      IS the packed file, so its path is the packed file's path),
//!   2. reads the trailer to locate the app segment (offset = `loader_len`),
//!   3. reads the `.upxz` container at that offset and decompresses it (zstd),
//!   4. writes the original binary to a temp file, `chmod 0500`, and ad-hoc
//!      re-signs it (AMFI kills an unsigned copy on exec),
//!   5. `execv`s the temp binary with the original stored name as `argv[0]` and
//!      the rest of `argv` forwarded verbatim.
//!
//! `execv` does not return, so the temp file cannot be unlinked in this
//! process. We deliberately do NOT fork a cleanup watchdog: forking from the
//! ad-hoc-signed no_std loader before execv reliably triggers AMFI SIGKILL
//! (exit 137) on the exec'd program (empirically verified). Each run leaves a
//! `/tmp/upxz-app-<pid>` behind (harmless: owner-only, chmod 0500, cleared on
//! reboot). See mneme `docs/upxz/` for the full design.
//!
//! Size is a hard gate: the loader must be < 1/5 of the full upxz binary
//! (~300 KB) and we target < 100 KB. `no_std` + zstd-sys FFI lands at ~84 KB;
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

// ---- macOS libc subset for I/O + exec + self-path resolution. ----
extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
    fn chmod(path: *const c_char, mode: u32) -> c_int;
    fn execv(path: *const c_char, argv: *const *const c_char) -> c_int;
    fn fork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn unlink(path: *const c_char) -> c_int;
    fn getpid() -> c_int;
    fn _exit(code: c_int) -> !;
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
    // `_NSGetExecutablePath` (libSystem dyld API) resolves the path of the
    // currently-executing binary, independent of argv[0]. argv[0] can be a
    // presentation name (e.g. set by execv in another program) or a relative
    // path that no longer resolves; the dyld call always returns the real
    // on-disk path the kernel exec'd. Symbol is `__NSGetExecutablePath` in the
    // C namespace. Returns 0 on success, -1 if the buffer is too small (and
    // sets *bufsize to the required size).
    fn _NSGetExecutablePath(buf: *mut c_char, bufsize: *mut u32) -> c_int;
    // `realpath` resolves a (possibly relative / symlinked) path to an absolute
    // canonical path. We use it on the dyld result to get an absolute path for
    // open(). It is in libSystem (stdlib).
    fn realpath(path: *const c_char, resolved: *mut c_char) -> *mut c_char;
}

const O_RDONLY: c_int = 0;
const O_WRONLY: c_int = 1;
const O_CREAT: c_int = 0o100;
const O_TRUNC: c_int = 0o1000;
const SEEK_SET: c_int = 0;
const SEEK_END: c_int = 2;

// Trailer (16 bytes): magic(8) + loader_len(4 BE) + app_len(4 BE).
// Two-segment design: there is no boot segment, so the trailer records only
// the loader length (where the app segment begins) and the app length.
// 8 + 4 + 4 = 16 bytes. mneme `docs/upxz/trailer.md` specifies this layout.
const TRAILER_LEN: usize = 16;
// 8-byte trailer magic: `UPXZEND1` (UPXZ end-of-packed, version 1). The
// trailing `1` is a format version marker; a future incompatible trailer can
// bump it.
const MAGIC_TRAILER: &[u8; 8] = b"UPXZEND1";

// upxz container magic prefix: ASCII `UPXZ` + version `0x01` (5 bytes). The
// full 8-byte magic is `[UPXZ\x01][codec][0x00][0x00]`; the codec byte at
// offset 5 (CODEC_OFFSET) selects the decompressor. This loader is zstd-only
// (codec id 0) for size: it is no_std + zstd-sys FFI and cannot carry a gzip
// decoder without blowing the < 100 KB gate. A gzip container (codec id 1) is
// detected here and rejected with a clear message — pack with zstd (drop --gz)
// or use the cross-platform `upxz run` runner path instead. Matches
// `upxz::format::MAGIC_PREFIX` / `CODEC_OFFSET`.
const MAGIC_UPXZ_PREFIX: &[u8; 5] = b"UPXZ\x01";
const CODEC_OFFSET: usize = 5;
const CODEC_ZSTD: u8 = 0;
const CODEC_GZIP: u8 = 1;
// The full magic length is 8 (prefix + codec + 2 reserved). We still need this
// to compute the name_len offset below.
const UPXZ_MAGIC_LEN: usize = 8;
const UPXZ_HEADER_FIXED: usize = UPXZ_MAGIC_LEN + 4; // magic + name_len

// zstd content-size sentinels.
const ZSTD_CONTENTSIZE_UNKNOWN: u64 = u64::MAX;
const ZSTD_CONTENTSIZE_ERROR: u64 = u64::MAX - 1;

/// Exit codes (synced with conventional sysexits.h-ish + sh conventions).
#[allow(dead_code)] // EXIT_USAGE unused in the two-segment design (loader reads self, no argv gate)
const EXIT_USAGE: c_int = 64; // bad usage / missing argv
const EXIT_IO: c_int = 74; // I/O error
const EXIT_FORMAT: c_int = 65; // bad container / trailer
const EXIT_DECOMPRESS: c_int = 76; // zstd failure
const EXIT_EXEC: c_int = 127; // execve failed
const EXIT_PANIC: c_int = 70; // panic (should not happen; no_std aborts)

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

// This is the kernel C entry point (`#[no_mangle] extern "C" main`); it must
// take `argv` as a raw pointer. Clippy's `not_unsafe_ptr_arg_deref` would have
// us mark the function `unsafe`, but the kernel calls it directly — there is no
// safer signature. We suppress the lint and document the invariant in
// `real_main` instead.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn main(argc: c_int, argv: *const *const c_char) -> c_int {
    // SAFETY: `argv` is the C argv passed by the kernel; we only read it within
    // bounds (guarded by `argc`) and pass it straight to `real_main` which treats
    // it identically. All FFI calls inside happen before `execv` returns.
    unsafe { real_main(argc, argv) }
}

unsafe fn real_main(argc: c_int, argv: *const *const c_char) -> c_int {
    // argv layout (two-segment design — the loader IS the packed file):
    //   argv[0] = packed file path (as invoked, e.g. "./packed")
    //   argv[1..] = user args, forwarded verbatim to the restored binary
    //
    // We do NOT trust argv[0] for self-reading: it can be a presentation name
    // or a stale relative path. Instead we resolve the real executable path
    // via `_NSGetExecutablePath`, which the kernel set when it exec'd us.
    let _ = argc; // user argc is recomputed below from argv (we forward argv[1..]).

    // Resolve our own on-disk path. `_NSGetExecutablePath` may return a relative
    // path; `realpath` canonicalises it to an absolute one for `open`.
    // PATH_MAX is 1024 on macOS; we size generously. NOTE: both `self_buf` and
    // `canon` live in THIS scope (not inside the `if` block) because
    // `packed_path` is a raw pointer into one of them and must remain valid
    // through the `open` calls below — a block-local buffer would dangle.
    let mut self_buf = [0u8; 4096];
    let mut canon = [0u8; 4096];
    let mut self_len: u32 = self_buf.len() as u32;
    let packed_path: *const c_char = if _NSGetExecutablePath(
        self_buf.as_mut_ptr() as *mut c_char,
        &mut self_len as *mut u32,
    ) == 0
    {
        // Success: self_buf holds a possibly-relative path. Canonicalise it.
        let r = realpath(
            self_buf.as_ptr() as *const c_char,
            canon.as_mut_ptr() as *mut c_char,
        );
        if r.is_null() {
            // realpath failed (unlikely on an executing file); fall back to the
            // raw dyld path.
            self_buf.as_ptr() as *const c_char
        } else {
            canon.as_ptr() as *const c_char
        }
    } else {
        // dyld failed entirely (extremely rare). Fall back to argv[0].
        *argv.offset(0)
    };

    // --- 1. Open the packed file (== ourselves) and size it via lseek. ---
    let fd = open(packed_path, O_RDONLY);
    if fd < 0 {
        warn(2, b"upxz-loader: cannot open packed file\n");
        return EXIT_IO;
    }
    // Size the file with `lseek(.., 0, SEEK_END)`, NOT `fstat` + `st_size`.
    // The raw `fstat` symbol a Rust `extern` resolves to on x86_64 macOS is the
    // LEGACY 32-bit-inode variant (`fstat`, not `fstat$INODE64` the C headers
    // alias to); it writes a DIFFERENT, smaller `struct stat`, so `st_size`
    // lands at the wrong offset and reads 0 — the packed file then looks "too
    // small" and every x86_64 SFX fails at runtime. arm64 has no legacy symbol
    // (its `fstat` is already inode64), which is why this only breaks for
    // x86_64-apple-darwin. A hand-rolled `struct stat` is the fragile part, so
    // we drop it entirely: `lseek` to EOF gives the byte size portably with no
    // struct-ABI dependency. The trailer/app reads below seek absolute offsets,
    // so we never need to restore the cursor after this.
    let end = lseek(fd, 0, SEEK_END);
    if end < 0 {
        warn(2, b"upxz-loader: cannot seek packed file\n");
        close(fd);
        return EXIT_IO;
    }
    let total = end as usize;
    if total < TRAILER_LEN {
        warn(
            2,
            b"upxz-loader: packed file too small to contain a trailer\n",
        );
        close(fd);
        return EXIT_FORMAT;
    }

    // --- 2. Read trailer (last 16 bytes) and validate magic. ---
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
        warn(
            2,
            b"upxz-loader: bad trailer magic (not an upxz packed file)\n",
        );
        return EXIT_FORMAT;
    }
    // Two-segment trailer: magic(8) + loader_len(4 BE) + app_len(4 BE).
    let loader_len = be_u32(&trailer[8..12]) as usize;
    let app_len = be_u32(&trailer[12..16]) as usize;
    // The loader segment is bytes [0, loader_len); the app segment is
    // [loader_len, loader_len + app_len); the trailer is the last 16 bytes.
    let app_start = loader_len;
    let app_end = app_start.saturating_add(app_len);
    if app_start > total || app_end > total - TRAILER_LEN {
        warn(
            2,
            b"upxz-loader: trailer segment lengths are inconsistent with file size\n",
        );
        return EXIT_FORMAT;
    }

    // --- 3. Read just the .upxz app segment (offset read; never write it out). ---
    let app_buf = malloc(app_len) as *mut u8;
    if app_buf.is_null() {
        warn(
            2,
            b"upxz-loader: out of memory allocating app segment buffer\n",
        );
        return EXIT_IO;
    }
    let fd = open(packed_path, O_RDONLY);
    if fd < 0 {
        warn(
            2,
            b"upxz-loader: cannot reopen packed file for app segment\n",
        );
        free(app_buf as *mut c_void);
        return EXIT_IO;
    }
    if lseek(fd, app_start as i64, SEEK_SET) < 0 || !read_exact(fd, app_buf, app_len) {
        warn(2, b"upxz-loader: cannot read app segment\n");
        close(fd);
        free(app_buf as *mut c_void);
        return EXIT_IO;
    }
    close(fd);

    // --- 4. Parse the .upxz container header: magic + name_len(4 BE) + name. ---
    if app_len < UPXZ_HEADER_FIXED {
        warn(
            2,
            b"upxz-loader: app segment too small to be a .upxz container\n",
        );
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    // Validate the 5-byte magic prefix, the reserved bytes (must be 0), and
    // the codec byte. The loader is zstd-only; a gzip container is rejected
    // here with a distinct message so the user knows to repack or use the
    // runner path.
    if core::slice::from_raw_parts(app_buf, MAGIC_UPXZ_PREFIX.len()) != *MAGIC_UPXZ_PREFIX {
        warn(
            2,
            b"upxz-loader: app segment does not start with the upxz magic prefix\n",
        );
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    // Reserved bytes at offsets 6, 7 must be zero.
    if *app_buf.add(MAGIC_UPXZ_PREFIX.len() + 1) != 0
        || *app_buf.add(MAGIC_UPXZ_PREFIX.len() + 2) != 0
    {
        warn(
            2,
            b"upxz-loader: bad magic (reserved bytes non-zero; not a known upxz format)\n",
        );
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    let codec_byte = *app_buf.add(CODEC_OFFSET);
    if codec_byte == CODEC_GZIP {
        // The no_std loader is zstd-only for size; it cannot decode gzip. The
        // user should repack with zstd (drop --gz) or run via the external
        // `upxz` runner, which supports both codecs.
        warn(
            2,
            b"upxz-loader: container uses gzip codec, but the macOS loader is zstd-only; \
             repack without --gz or run via `upxz <file>.upxz`\n",
        );
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    if codec_byte != CODEC_ZSTD {
        warn(
            2,
            b"upxz-loader: unknown upxz codec byte in magic (not zstd or gzip)\n",
        );
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    let name_len = be_u32(core::slice::from_raw_parts(app_buf.add(8), 4)) as usize;
    let payload_start = UPXZ_HEADER_FIXED.saturating_add(name_len);
    if payload_start > app_len {
        warn(
            2,
            b"upxz-loader: declared name length runs past the container\n",
        );
        free(app_buf as *mut c_void);
        return EXIT_FORMAT;
    }
    // NOTE: the stored name is copied below into `name_cstr_bytes` (a separate
    // heap buffer with a trailing NUL), so app_buf does NOT need to outlive the
    // copy. Both app_buf and the decompression scratch dst are freed before
    // execv; name_cstr_bytes is held until execv because argv[0] points at it.
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
    let mut out_size: usize =
        if declared == ZSTD_CONTENTSIZE_UNKNOWN || declared == ZSTD_CONTENTSIZE_ERROR {
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
        warn(
            2,
            b"upxz-loader: cannot create zstd decompression context\n",
        );
        free(app_buf as *mut c_void);
        return EXIT_DECOMPRESS;
    }
    let mut dst: *mut u8 = core::ptr::null_mut();
    #[allow(unused_assignments)] // reassigned across retry loop iterations
    let mut got: usize = 0;
    loop {
        if out_size > MAX_OUT {
            warn(
                2,
                b"upxz-loader: decompressed size exceeds 1 GiB sanity cap\n",
            );
            ZSTD_freeDCtx(dctx);
            if !dst.is_null() {
                free(dst as *mut c_void);
            }
            free(app_buf as *mut c_void);
            return EXIT_DECOMPRESS;
        }
        dst = malloc(out_size) as *mut u8;
        if dst.is_null() {
            warn(
                2,
                b"upxz-loader: out of memory allocating decompression buffer\n",
            );
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
        warn(
            2,
            b"upxz-loader: cannot create temp file for restored binary\n",
        );
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
    //   - macOS clears /tmp on reboot, so the directory does not grow without
    //     bound across reboots. Users who want them gone sooner can
    //     `rm /tmp/upxz-app-*` manually.
    // This trade (one residual temp file per run) is the standard one
    // `/tmp`-based tooling makes when in-memory exec is unavailable (which on
    // macOS it is — see mneme shm PoC).

    // --- 9. Build exec argv and execv the restored binary. ---
    // argv[0] = the stored original name (so the program sees its real name);
    // argv[1..] = the loader's argv[1..] forwarded verbatim (the loader IS the
    // packed file, so its argv[0] is the packed path and argv[1..] are exactly
    // the user args). The temp path is NOT placed in argv[0]; argv[0] is a
    // presentation name only.
    let user_argc = (argc - 1).max(0) as usize;
    let argv_total = user_argc + 2; // [name, user_args..., NULL]
    let argv_bytes = argv_total * core::mem::size_of::<*const c_char>();
    let exec_argv = malloc(argv_bytes) as *mut *const c_char;
    if exec_argv.is_null() {
        warn(2, b"upxz-loader: out of memory building exec argv\n");
        free(name_cstr_bytes as *mut c_void);
        return EXIT_IO;
    }
    // argv[0] = stored name.
    *exec_argv.add(0) = name_cstr_bytes as *const c_char;
    // argv[1..] = forward loader argv[1..argc) verbatim.
    let mut i = 1;
    while i - 1 < user_argc {
        // loader argv index = 1 + (i - 1) = i
        *exec_argv.add(i) = *argv.add(i);
        i += 1;
    }
    *exec_argv.add(user_argc + 1) = core::ptr::null();

    // execv replaces this process image. stdin/stdout/stderr are inherited
    // unchanged (we never touched them), so the restored binary sees the same
    // stdio the user gave `./packed`.
    execv(tmp_path.as_ptr() as *const c_char, exec_argv);
    // execv only returns on failure.
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
