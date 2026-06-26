//! `upxz --bin`: run a single entry directly out of a `.tar.zst` archive,
//! **without extracting the whole archive to disk**.
//!
//! This is AppImage-style distribution: the archive is the distribution unit
//! and one designated inner binary is executed from it.
//!
//! ## Mechanism
//!
//! 1. Open the archive and wrap it in a streaming zstd `Decoder`
//!    (`BufReader<File> → Decoder → tar::Archive`).
//! 2. Stream tar entries one at a time. Each non-matching entry is read and
//!    discarded; nothing is written to disk. This bounds disk usage to the
//!    single inner binary regardless of archive size.
//! 3. On the matching entry (`path == inner_path`, regular file), collect its
//!    bytes into memory and stop scanning.
//! 4. Materialize the inner bytes:
//!    - **Linux**: write to a `memfd_create` file and `fexecve` — the binary
//!      lives only in memory, never on disk.
//!    - **macOS**: write to a temp file under `$TMPDIR`, ad-hoc codesign it
//!      (AMFI would SIGKILL a copied signed Mach-O otherwise), chmod 0o500,
//!      then `execvp`. The temp file is removed after the child exits.
//! 5. `argv[0]` is set to the inner binary's basename; args after `--` are
//!    forwarded verbatim. The child's exit code is propagated.
//!
//! ## Why not the `tar` crate's `unpack`?
//!
//! `tar::Archive::unpack` writes every entry to disk. We only want one, so we
//! drive `entries()` ourselves and read only the match.

use anyhow::{bail, ensure, Context, Result};
use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;

/// Run `inner_path` from `archive` (a `.tar.zst`). See module docs for the
/// streaming + exec strategy. `trailing` is forwarded verbatim as argv to the
/// inner binary.
pub fn run(archive: &Path, inner_path: &str, quiet: bool, trailing: &[String]) -> Result<()> {
    ensure!(
        !inner_path.is_empty(),
        "--bin requires a non-empty inner path"
    );
    // Normalize the request: strip a leading './' so that `--bin ./bin/hello`
    // matches a tar entry stored as `bin/hello` (and vice-versa). Tar entries
    // are conventionally relative and may or may not carry the leading `./`.
    let want = normalize_tar_path(inner_path);

    let f = fs::File::open(archive)
        .with_context(|| format!("cannot open archive {}", archive.display()))?;
    let meta = f
        .metadata()
        .with_context(|| format!("cannot stat archive {}", archive.display()))?;
    ensure!(
        meta.is_file(),
        "archive {} is not a regular file",
        archive.display()
    );

    // Streaming zstd decode over a buffered reader. `Decoder` pulls from the
    // inner reader on demand, so we never hold the whole archive in memory.
    let decoder = zstd::Decoder::new(BufReader::new(f))
        .with_context(|| format!("zstd: cannot start decoding {}", archive.display()))?;
    let mut tar = tar::Archive::new(decoder);

    let inner_bytes = find_entry(&mut tar, &want)
        .with_context(|| format!("did not find entry {:?} in {}", want, archive.display()))?;

    // argv[0]: basename of the inner path, per exec convention.
    let argv0 = Path::new(inner_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| inner_path.to_owned());

    if !quiet {
        eprintln!(
            "bin {} from {} ({} bytes, exec {})",
            inner_path,
            archive.display(),
            inner_bytes.len(),
            argv0
        );
    }

    exec_inner(&argv0, &inner_bytes, trailing)
}

/// Read tar entries one at a time, returning the bytes of the first regular
/// file whose normalized path equals `want`. All other entries are read and
/// discarded (never written to disk).
fn find_entry<R: Read>(tar: &mut tar::Archive<R>, want: &str) -> Result<Vec<u8>> {
    for entry in tar.entries()? {
        let mut entry = entry.context("tar: error while reading an entry")?;
        let header_path = entry.path().context("tar: entry has an invalid path")?;
        let got = normalize_tar_path(&header_path.display().to_string());
        if got != want {
            // Drain and discard so the tar reader stays aligned; without this
            // the next `entries()` call would see a corrupt stream.
            std::io::copy(&mut entry, &mut std::io::sink())
                .context("tar: error while skipping an entry")?;
            continue;
        }
        ensure!(
            entry.header().entry_type().is_file(),
            "entry {:?} is not a regular file (type {:?}); --bin only runs files",
            want,
            entry.header().entry_type()
        );
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .context("tar: error while reading the matched entry")?;
        return Ok(buf);
    }
    bail!("no tar entry named {:?} (looked for regular file)", want);
}

/// Canonicalize a tar entry path for comparison: drop a leading `./` and any
/// trailing `/`. Tar entries are relative; this makes `bin/hello`, `./bin/hello`
/// and `bin/hello/` compare equal. Bare `.` and `./` normalize to the empty
/// string (no real entry name).
fn normalize_tar_path(p: &str) -> String {
    let mut s = p;
    if s.starts_with("./") {
        s = &s[2..];
    }
    s.trim_end_matches('/').to_owned()
}

/// Materialize `bytes` as an executable and `exec` it with `argv[0]=name` and
/// the given trailing args. The child's exit code is propagated; this function
/// does not return on success.
fn exec_inner(name: &str, bytes: &[u8], trailing: &[String]) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        return exec_memfd_linux(name, bytes, trailing);
    }
    #[cfg(target_os = "macos")]
    {
        return exec_temp_macos(name, bytes, trailing);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (name, bytes, trailing);
        bail!("upxz --bin is only supported on Linux and macOS");
    }
}

// ---------------------------------------------------------------------------
// Linux: memfd_create + fexecve — the inner binary never touches disk.
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn exec_memfd_linux(name: &str, bytes: &[u8], trailing: &[String]) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // memfd_create with a name that is purely advisory. Close-on-exec is set
    // so the fd does not leak into the child (we use fexecve, which consumes
    // it during the syscall rather than via /proc/self/fd).
    let mem_name = CString::new("upxz-bin").unwrap();
    let fd = unsafe { libc::memfd_create(mem_name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("memfd_create failed");
    }
    // Wrap the fd so it is closed if any step below fails.
    let owned = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    let mut written = 0usize;
    while written < bytes.len() {
        let n = unsafe {
            libc::write(
                fd,
                bytes[written..].as_ptr() as *const _,
                bytes.len() - written,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error()).context("memfd write failed");
        }
        written += n as usize;
    }
    // From here we hand off to fexecve; drop the Rust guard without closing
    // (fexecve needs the fd live, and on success the syscall does not return).
    std::mem::forget(owned);

    // Build argv: [name, trailing...].
    let mut argv_c: Vec<CString> = Vec::with_capacity(1 + trailing.len());
    argv_c.push(CString::new(name).context("argv[0] contains a NUL byte")?);
    for a in trailing {
        argv_c.push(CString::new(a.as_bytes()).context("trailing arg contains a NUL byte")?);
    }
    let mut argv_p: Vec<*const libc::c_char> = argv_c.iter().map(|s| s.as_ptr()).collect();
    argv_p.push(std::ptr::null());

    // Inherit the current environment.
    let envp: Vec<*const libc::c_char> = {
        let envs: Vec<CString> = std::env::vars_os()
            .map(|(k, v)| {
                let mut bytes = k.as_bytes().to_vec();
                bytes.push(b'=');
                bytes.extend_from_slice(v.as_bytes());
                CString::new(bytes).unwrap()
            })
            .collect();
        let mut ptrs: Vec<*const libc::c_char> = envs.iter().map(|s| s.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        std::mem::forget(envs); // keep alive until exec
        ptrs
    };

    // fexecve is the Linux in-memory exec. On success it does not return.
    let rc = unsafe { libc::fexecve(fd, argv_p.as_mut_ptr(), envp.as_mut_ptr() as *mut *mut _) };
    let err = std::io::Error::last_os_error();
    let _ = rc;
    // If we reach here, fexecve failed.
    // Workaround for musl/glibc without fexecve: fall back to /proc/self/fd.
    let proc_path = CString::new(format!("/proc/self/fd/{}", fd)).unwrap();
    let rc2 = unsafe {
        libc::execve(
            proc_path.as_ptr(),
            argv_p.as_mut_ptr(),
            envp.as_mut_ptr() as *mut _,
        )
    };
    let err2 = std::io::Error::last_os_error();
    let _ = rc2;
    bail!("fexecve failed ({err}); /proc/self/fd fallback also failed ({err2})");
}

// ---------------------------------------------------------------------------
// macOS: temp file + ad-hoc codesign (no in-memory exec on Darwin).
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn exec_temp_macos(name: &str, bytes: &[u8], trailing: &[String]) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!(".upxz-bin-{}-{}", std::process::id(), name));
    fs::write(&tmp, bytes)
        .with_context(|| format!("cannot write inner binary to {}", tmp.display()))?;
    // Ad-hoc re-sign: macOS AMFI SIGKILLs (exit 137) a copied signed Mach-O
    // on exec otherwise. `--force` overwrites the stale signature.
    let cs = std::process::Command::new("codesign")
        .args(["--sign", "-", "--force", "--"])
        .arg(&tmp)
        .output()
        .with_context(|| "failed to spawn codesign")?;
    if !cs.status.success() {
        let _ = fs::remove_file(&tmp);
        bail!(
            "codesign failed on {} (exit {:?}): {}",
            tmp.display(),
            cs.status.code(),
            String::from_utf8_lossy(&cs.stderr).trim()
        );
    }
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o500))
            .with_context(|| format!("cannot chmod inner binary {}", tmp.display()))?;
    }

    // std::process::Command sets argv[0] to the resolved program path. On
    // Linux the memfd path instead uses raw fexecve with argv[0]=basename, so
    // the two platforms differ slightly; most programs do not read argv[0].
    let status = std::process::Command::new(&tmp)
        .args(trailing)
        .status()
        .with_context(|| format!("failed to exec inner binary {}", tmp.display()))?;
    let _ = fs::remove_file(&tmp);
    // Propagate the child's exit code.
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_leading_dot_slash_and_trailing_slash() {
        assert_eq!(normalize_tar_path("./bin/hello"), "bin/hello");
        assert_eq!(normalize_tar_path("bin/hello"), "bin/hello");
        assert_eq!(normalize_tar_path("bin/hello/"), "bin/hello");
        assert_eq!(normalize_tar_path("./"), "");
        assert_eq!(normalize_tar_path(""), "");
    }
}
