//! upxz-stub — Linux self-extracting binary loader (Plan D).
//!
//! This binary is **not run directly by the user**. `upxz -c` prepends these
//! stub bytes to a `.upxz` container and writes a fixed-size trailer, producing
//! a self-contained executable `packed` whose layout is:
//!
//! ```text
//! [ stub ELF bytes ][ .upxz container (magic+namelen+name+payload) ][ trailer: u64 stub_size BE ]
//! ```
//!
//! When `packed` is executed the stub:
//!   1. reads `/proc/self/exe` (its own image),
//!   2. reads the last 8 bytes to learn `stub_size`,
//!   3. slices `.upxz` starting at `stub_size`,
//!   4. parses the header (magic + name) and reads the **codec id** out of
//!      the magic byte at offset 5 (0 = zstd, 1 = gzip),
//!   5. decompresses the payload through the matching backend
//!      (zstd::decode_all for zstd, flate2/miniz_oxide for gzip),
//!   6. creates a memfd (MFD_EXEC, no CLOEXEC), writes the original to it,
//!   7. `fexecve`s it with argv[0] = stored original name, forwarding argv[1..]
//!      and the inherited environment.
//!
//! No temp file is written to disk — the original lives only in memory for the
//! lifetime of the exec'd process. Linux-only. The stub supports both codecs
//! (zstd + gzip) because it is a normal std binary with no size gate, unlike
//! the macOS no_std loader which stays zstd-only.

use std::io::Read;

/// upxz container magic prefix: ASCII `UPXZ` + version `0x01` (5 bytes). The
/// full 8-byte magic is `[UPXZ\x01][codec][0x00][0x00]`; the codec byte at
/// offset 5 selects the decompressor. Kept in sync with
/// `upxz::format::MAGIC_PREFIX` / `CODEC_OFFSET`. Duplicated here so the stub
/// crate stays standalone (no cross-crate link into the packer).
const MAGIC_PREFIX: [u8; 5] = *b"UPXZ\x01";
const CODEC_OFFSET: usize = 5;

/// Codec ids (mirrors `upxz::format::Codec`). The stub dispatches on the byte
/// read from the magic.
const CODEC_ZSTD: u8 = 0;
const CODEC_GZIP: u8 = 1;

/// Trailer length, in bytes: a single big-endian u64 recording the stub size.
const TRAILER_LEN: usize = 8;

/// memfd_create flags. We use `MFD_EXEC` (0x10) only:
///   - `MFD_EXEC` marks the memfd executable, which is REQUIRED on Linux 6.3+
///     under `vm.memfd_noexec=1|2` (the secure default on hardened distros and
///     some container runtimes). Without it the memfd is rejected or sealed
///     non-exec and the later `fexecve` fails, breaking every SFX binary.
///   - We deliberately do NOT set `MFD_CLOEXEC`. A CLOEXEC memfd cannot be
///     exec'd as a `#!interpreter` script: binfmt_script needs to re-open the
///     script file by reference to pass it to the interpreter, and a
///     close-on-exec memfd makes that re-open fail with ENOENT. Since upxz
///     can pack scripts (not just ELF binaries), CLOEXEC would silently break
///     a whole class of inputs.
///   - On kernels older than 6.3 the `MFD_EXEC` flag is unknown and
///     `memfd_create` returns `EINVAL`; we retry with flags=0 (those kernels
///     default memfds to executable, which is what we want).
///
/// See `memfd_create(2)` and `Documentation/userspace-api/mfd_noexec.rst`.
const MFD_EXEC: u32 = 0x0010;

/// Create an executable memfd. Tries `MFD_EXEC` first (correct on modern
/// hardened kernels); if the kernel does not know the flag it returns `EINVAL`
/// and we retry with flags=0 (older kernels default memfds to executable).
/// Returns the fd on success or the last errno on failure.
fn create_exec_memfd() -> Result<std::os::fd::RawFd, i32> {
    let name_c = std::ffi::CString::new("upxz").unwrap();
    let try_flags = |flags: u32| -> Result<std::os::fd::RawFd, i32> {
        let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name_c.as_ptr(), flags) };
        if fd >= 0 {
            Ok(fd as std::os::fd::RawFd)
        } else {
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        }
    };
    match try_flags(MFD_EXEC) {
        Ok(fd) => Ok(fd),
        Err(errno) if errno == libc::EINVAL => try_flags(0),
        Err(e) => Err(e),
    }
}

fn main() -> ! {
    // Any error path: print to stderr and exit non-zero. We do NOT silently
    // fall through to a "no-op" because the stub has no useful behavior of its
    // own — if it runs, it must either exec or fail loudly.
    if let Err(err) = run() {
        eprintln!("upxz-stub: {err}");
        std::process::exit(127);
    }
    // run() only returns Ok if fexecve somehow returned 0 (it never does), so
    // treat that as a logic error too.
    eprintln!("upxz-stub: fexecve returned unexpectedly");
    std::process::exit(127);
}

fn run() -> Result<(), String> {
    // 1. Read our own image. `/proc/self/exe` is the canonical symlink to the
    //    running executable even when argv[0] has been tampered with.
    let mut image = Vec::new();
    std::fs::File::open("/proc/self/exe")
        .map_err(|e| format!("cannot open /proc/self/exe: {e}"))?
        .read_to_end(&mut image)
        .map_err(|e| format!("cannot read self image: {e}"))?;

    // 2. Trailer: last 8 bytes = stub size, big-endian.
    if image.len() < TRAILER_LEN {
        return Err("self image too small to contain a trailer".to_string());
    }
    let tail = &image[image.len() - TRAILER_LEN..];
    let stub_size = u64::from_be_bytes([
        tail[0], tail[1], tail[2], tail[3], tail[4], tail[5], tail[6], tail[7],
    ]) as usize;
    if stub_size + TRAILER_LEN > image.len() {
        return Err(format!(
            "declared stub size {stub_size} is larger than the image ({})",
            image.len()
        ));
    }

    // 3. The `.upxz` container lives between `stub_size` and the trailer.
    let upxz = &image[stub_size..image.len() - TRAILER_LEN];

    // 4. Parse header: magic prefix (5 bytes) + codec byte + 2 reserved bytes
    //    + 4-byte BE name length + name bytes, then the compressed payload.
    if upxz.len() < MAGIC_PREFIX.len() + 3 + 4 {
        return Err("trailer points at a region too small to be a .upxz container".to_string());
    }
    if upxz[..MAGIC_PREFIX.len()] != MAGIC_PREFIX {
        return Err("trailer region does not start with the upxz magic prefix".to_string());
    }
    // Reserved bytes (offsets 6, 7) must be zero today.
    if upxz[MAGIC_PREFIX.len() + 1] != 0 || upxz[MAGIC_PREFIX.len() + 2] != 0 {
        return Err("bad magic: reserved bytes are non-zero (not a known upxz format)".to_string());
    }
    let codec_byte = upxz[CODEC_OFFSET];
    if codec_byte != CODEC_ZSTD && codec_byte != CODEC_GZIP {
        return Err(format!(
            "unknown upxz codec id {codec_byte} in stub (only 0=zstd, 1=gzip are defined)"
        ));
    }
    let name_len = u32::from_be_bytes([
        upxz[MAGIC_PREFIX.len() + 3],
        upxz[MAGIC_PREFIX.len() + 4],
        upxz[MAGIC_PREFIX.len() + 5],
        upxz[MAGIC_PREFIX.len() + 6],
    ]) as usize;
    let name_start = MAGIC_PREFIX.len() + 3 + 4;
    let payload_start = name_start
        .checked_add(name_len)
        .ok_or("name length overflows usize")?;
    if payload_start > upxz.len() {
        return Err(format!(
            "declared name length {name_len} runs past the container"
        ));
    }
    let name = std::str::from_utf8(&upxz[name_start..payload_start])
        .map_err(|e| format!("stored original name is not valid UTF-8: {e}"))?;
    let payload = &upxz[payload_start..];

    // 5. Decompress. Dispatch on the codec byte in the magic: zstd for id 0,
    //    gzip (flate2/miniz_oxide) for id 1. Both backends are linked into the
    //    stub; the stub is a normal std binary with no size gate, so unlike the
    //    macOS no_std loader it can carry both decoders.
    let original = match codec_byte {
        CODEC_ZSTD => zstd::decode_all(payload)
            .map_err(|e| format!("zstd decompression failed: {e}"))?,
        CODEC_GZIP => {
            let mut dec = flate2::read::GzDecoder::new(payload);
            let mut out = Vec::new();
            dec.read_to_end(&mut out)
                .map_err(|e| format!("gzip decompression failed: {e}"))?;
            out
        }
        // Unreachable: codec_byte is validated above.
        other => return Err(format!("internal error: unhandled codec id {other}")),
    };

    // 6. memfd_create + write + fexecve. We request MFD_EXEC so the memfd is
    //    executable on hardened kernels (vm.memfd_noexec). We do NOT set
    //    MFD_CLOEXEC because it breaks `#!script` exec from a memfd (the
    //    kernel cannot re-open a close-on-exec memfd to pass it to the
    //    script interpreter). See `create_exec_memfd` for the full rationale.
    let fd = create_exec_memfd().map_err(|errno| format!("memfd_create failed (errno {errno})"))?;

    // Write the original bytes. write() may be partial, so loop.
    let mut written = 0usize;
    while written < original.len() {
        let n = unsafe {
            libc::write(
                fd,
                original[written..].as_ptr() as *const libc::c_void,
                original.len() - written,
            )
        };
        if n < 0 {
            return Err(format!(
                "write to memfd failed (errno {})",
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
            ));
        }
        written += n as usize;
    }

    // 7. Build argv[0] = stored name, forward argv[1..] verbatim.
    let mut argv: Vec<std::ffi::CString> = Vec::with_capacity(std::env::args().len());
    argv.push(std::ffi::CString::new(name).map_err(|_| "stored name contains an interior NUL")?);
    for a in std::env::args().skip(1) {
        argv.push(std::ffi::CString::new(a).map_err(|_| "an argument contains an interior NUL")?);
    }
    let argv_ptrs: Vec<*const libc::c_char> = argv
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // envp: pass the current environment through unchanged. POSIX env vars are
    // NUL-free in practice, but map_err (not .expect) keeps the documented
    // "print to stderr and exit 127" contract if a constructed env ever
    // contains one.
    let env: Vec<std::ffi::CString> = std::env::vars()
        .map(|(k, v)| {
            std::ffi::CString::new(format!("{k}={v}"))
                .map_err(|_| "an environment variable contains an interior NUL".to_string())
        })
        .collect::<Result<_, _>>()?;
    let env_ptrs: Vec<*const libc::c_char> = env
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // fexecve replaces this process image. On success it does not return.
    // Signature: fexecve(fd, argv: *const *const c_char, envp: *const *const c_char).
    let rc = unsafe { libc::fexecve(fd, argv_ptrs.as_ptr(), env_ptrs.as_ptr()) };
    let _ = rc; // always < 0 if we reach here
    Err(format!(
        "fexecve failed (errno {})",
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    ))
}
