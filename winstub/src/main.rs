//! upxz-winstub — Windows self-extracting binary stub for upxz.
//!
//! This binary is **not run directly by the user**. `upxz -c` (built on
//! Windows) prepends these stub bytes to a `.upxz` container and writes a
//! fixed-size trailer, producing a self-contained executable `packed` whose
//! layout mirrors the Linux stub:
//!
//! ```text
//! [ stub PE bytes ][ .upxz container (magic+namelen+name+payload) ][ trailer: u64 stub_size BE ]
//! ```
//!
//! When `packed` is executed the stub:
//!   1. resolves its own executable path (`GetModuleFileNameW`),
//!   2. reads the whole image into memory,
//!   3. reads the last 8 bytes to learn `stub_size`,
//!   4. slices `.upxz` starting at `stub_size`,
//!   5. parses the header (magic + name) and reads the **codec id** out of
//!      the magic byte at offset 5 (0 = zstd, 1 = gzip),
//!   6. decompresses the payload through the matching backend,
//!   7. execs the original bytes.
//!
//! ## Exec path: temp file + CreateProcessW (the implemented route)
//!
//! Windows has no portable in-memory exec (no `memfd_create`/`fexecve`
//! analogue that "just works" on an arbitrary PE buffer — see the section on
//! the NT-section route below). The stub therefore writes the restored bytes
//! to `%TEMP%\upxz-<pid>-<tag>-<stem>.exe` and `CreateProcessW`s it, then
//! removes the temp file after the child exits. This is the same disk-drop
//! trade-off the macOS upxz-loader makes.
//!
//! Notably Windows does **not** require ad-hoc code-signing for a local exec
//! (unlike macOS AMFI, which SIGKILLs an unsigned copy). So unlike the macOS
//! path there is no re-sign step here. Windows Defender / SmartScreen may
//! prompt on the first run of an unknown .exe — that is the host's normal
//! behaviour for any newly-materialised binary and is not something upxz can
//! or should bypass.
//!
//! ## The NT-section route (in-memory exec): documented, not compiled
//!
//! Windows *does* expose a way to start a process directly from a memory
//! section (`NtCreateSection` + `NtMapViewOfSection` +
//! `NtCreateProcessEx`), which is the closest analogue to Linux
//! `memfd_create` + `fexecve` and would avoid the temp file entirely. It is
//! **not implemented here** for two concrete reasons:
//!
//! 1. `windows-sys` 0.59 exposes `NtCreateSection` and `NtMapViewOfSection`,
//!    but **not `NtCreateProcessEx`** (the process-from-section call). Using
//!    it would require hand-rolled FFI against `ntdll.dll` or the `ntapi`
//!    crate, and the resulting code is fragile across SDK versions.
//! 2. Even with the FFI in place, a process created from a section arrives
//!    with **no initial thread, no stack, and no PEB setup**. Running an
//!    arbitrary PE then requires manual import-table resolution, base
//!    relocation, PEB construction, and thread-context synthesis — i.e. a
//!    hand-rolled loader. This is substantially more code (and more
//!    Defender/AMSI surface) than the temp-file route, and it is exactly the
//!    technique AV products most aggressively flag.
//!
//! Because the develop host is macOS and cannot run Windows to validate any of
//! that, the in-memory route is left as a documented PoC in mneme
//! `docs/upxz/windows.md`. The temp-file path is the supported Windows SFX
//! mechanism. Revisit when there is a Windows host to iterate on.
//!
//! ## Trailer format (identical to the Linux stub)
//!
//! The trailer is a single big-endian `u64` recording the stub's byte length,
//! appended as the last 8 bytes of `packed`. This is byte-for-byte the same
//! shape as the Linux SFX (`[stub][.upxz][u64 stub_size BE]`), so a `.upxz`
//! container is portable across the two SFX packers without translation.
//!
//! ## Build note
//!
//! Like the sibling Linux `upxz-stub` crate, this crate is **target-locked**:
//! it uses `windows-sys` FFI throughout and does not compile on a non-Windows
//! host. `build.rs` only builds it when `CARGO_CFG_TARGET_OS == "windows"`,
//! mirroring how the Linux stub is only built on Linux. A `cargo check` of the
//! whole workspace on, say, macOS will fail to type-check this crate (no
//! `windows-sys` to resolve) — that is expected and matches the existing
//! behaviour for `upxz-stub` on macOS. The supported way to validate the
//! Windows stub is to build (or cross-build) for a Windows target.

use std::ffi::OsString;
use std::io::Read;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::Command;

/// upxz container magic prefix: ASCII `UPXZ` + version `0x01` (5 bytes). The
/// full 8-byte magic is `[UPXZ\x01][codec][0x00][0x00]`; the codec byte at
/// offset 5 selects the decompressor. Kept in sync with
/// `upxz::format::MAGIC_PREFIX` / `CODEC_OFFSET`. Duplicated here so the stub
/// crate stays standalone (no cross-crate link into the packer).
const MAGIC_PREFIX: [u8; 5] = *b"UPXZ\x01";
const CODEC_OFFSET: usize = 5;

/// Codec ids (mirrors `upxz::format::Codec`).
const CODEC_ZSTD: u8 = 0;
const CODEC_GZIP: u8 = 1;

/// Trailer length, in bytes: a single big-endian u64 recording the stub size.
/// Identical to the Linux stub trailer so a Windows-packed file and a
/// Linux-packed file share the same `[stub][.upxz][u64 stub_size BE]` shape.
const TRAILER_LEN: usize = 8;

fn main() -> ! {
    if let Err(err) = run() {
        eprintln!("upxz-winstub: {err}");
        std::process::exit(127);
    }
    // run() returns Ok only via the child-exit path, which calls exit() itself;
    // reaching this point is a logic error.
    eprintln!("upxz-winstub: exec path returned unexpectedly");
    std::process::exit(127);
}

fn run() -> Result<(), String> {
    // 1. Resolve and read our own image. `GetModuleFileNameW` returns the path
    //    of the .exe that loaded this process, which IS the packed file.
    let exe_path = current_exe_path()?;
    let image = std::fs::read(&exe_path)
        .map_err(|e| format!("cannot read self image {}: {e}", exe_path.display()))?;

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
    //    stub; the stub is a normal std binary with no size gate.
    let original = match codec_byte {
        CODEC_ZSTD => {
            zstd::decode_all(payload).map_err(|e| format!("zstd decompression failed: {e}"))?
        }
        CODEC_GZIP => {
            let mut dec = flate2::read::GzDecoder::new(payload);
            let mut out = Vec::new();
            dec.read_to_end(&mut out)
                .map_err(|e| format!("gzip decompression failed: {e}"))?;
            out
        }
        other => return Err(format!("internal error: unhandled codec id {other}")),
    };

    // 6. Temp-file exec: the supported Windows route. See the module docs for
    //    why the NT-section in-memory route is not compiled here.
    exec_via_temp_file(&original, name)
}

/// Temp-file exec path: write `original` to `%TEMP%`, then `CreateProcessW` it
/// (via `std::process::Command`) with argv forwarded verbatim. The temp file
/// is removed after the child exits.
///
/// argv[0] is left as the temp .exe path (the `CreateProcessW` default). The
/// stored original name is still preserved in the `.upxz` container header,
/// so `upxz -d` round-trips correctly; we do not override argv[0] because
/// `std::os::windows::process::CommandExt` does not expose `arg0` (only
/// `raw_arg`, and `CreateProcessW`'s default argv[0] = program path is what
/// Windows programs expect). Windows does not require ad-hoc code-signing for
/// a local exec (unlike macOS AMFI), so there is no re-sign step here.
fn exec_via_temp_file(original: &[u8], stored_name: &str) -> Result<(), String> {
    // Build a temp path under %TEMP%. We keep the stored name's stem so the
    // child sees a sensible argv[0] / process name, but always force a `.exe`
    // suffix (Windows exec is suffix-driven) and suffix a unique tag to avoid
    // collisions with concurrent or stale runs.
    let tmp_dir = std::env::temp_dir();
    let safe_stem = sanitize_filename_stem(stored_name);
    let tmp_name = format!(
        "upxz-{}-{}-{safe_stem}.exe",
        std::process::id(),
        unique_tag()
    );
    let tmp_path = tmp_dir.join(tmp_name);

    std::fs::write(&tmp_path, original).map_err(|e| {
        format!(
            "cannot write restored binary to {}: {e}",
            tmp_path.display()
        )
    })?;

    // Forward argv verbatim. We do NOT override argv[0] on Windows: the
    // `std::os::windows::process::CommandExt` trait exposes `raw_arg` for
    // that, but `CreateProcessW` semantics make argv[0] = the program path the
    // expected default, and most Windows programs inspect `GetCommandLineW`
    // rather than argv[0] anyway. The stored original name is still preserved
    // in the `.upxz` container header (so `upxz -d` round-trips correctly).
    //
    // SAFETY: the command program is the temp .exe we just wrote; we spawn it
    // directly (no shell), so there is no shell-injection surface.
    let mut cmd = Command::new(&tmp_path);
    let our_args: Vec<String> = std::env::args().skip(1).collect();
    cmd.args(&our_args);

    let status = cmd
        .status()
        .map_err(|e| format!("failed to exec restored binary {}: {e}", tmp_path.display()));

    // Best-effort cleanup; ignore errors (file may still be mapped/in use).
    let _ = std::fs::remove_file(&tmp_path);

    let status = status?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Resolve the path of our own executable via `GetModuleFileNameW`. This is
/// the packed file's path even when argv[0] has been tampered with.
fn current_exe_path() -> Result<PathBuf, String> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    const INITIAL_CAP: usize = 1024;
    let mut buf = vec![0u16; INITIAL_CAP];
    let n = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return Err("GetModuleFileNameW returned 0".to_string());
    }
    if n as usize == buf.len() {
        // Buffer was exactly filled -> the real path may be longer. Grow + retry.
        buf = vec![0u16; buf.len() * 2];
        let m =
            unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
        if m == 0 || m as usize == buf.len() {
            return Err("GetModuleFileNameW path was truncated even after growth".to_string());
        }
        buf.truncate(m as usize);
    } else {
        buf.truncate(n as usize);
    }
    Ok(PathBuf::from(OsString::from_wide(&buf)))
}

/// Flatten a stored name to a filesystem-safe stem (drop path separators and
/// the final extension; replace anything else surprising with `_`). Windows
/// forbids `< > : " / \ | ? *` and trailing dots/spaces in filenames.
fn sanitize_filename_stem(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Strip any extension; we force `.exe` ourselves.
    if let Some(dot) = s.rfind('.') {
        s.truncate(dot);
    }
    if s.is_empty() {
        s.push_str("app");
    }
    // Windows forbids trailing dots/spaces.
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    s
}

/// A short, mostly-unique tag (low-resolution monotonic time + a tiny PRNG
/// seeded from a local address) to avoid temp-file collisions across
/// concurrent runs of the same packed binary.
fn unique_tag() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let seed = nanos as usize ^ (std::ptr::addr_of!(nanos) as usize);
    // Cheap xorshift over the seed for ~4 hex chars.
    let mut x = seed.wrapping_mul(0x9E37_79B9).wrapping_add(1);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    format!("{x:04x}")
}
