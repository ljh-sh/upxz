//! upxz — upx using zstd. A packer that turns a file into a **self-extracting
//! executable** (upx-style: the output still runs).
//!
//! - `upxz foo`        → if foo is a plain file: **pack** to a self-extractor
//!   `foo.upxz` (chmod +x). `./foo.upxz` runs the original directly.
//! - `upxz foo.upxz`   → **refused**: foo.upxz is already packed. Run it
//!   directly (`./foo.upxz`) or restore the original with `-d`.
//! - `upxz -d foo.upxz`→ **unpack**: restore the original (executable bit
//!   restored when the original was an executable).
//! - `upxz -l/-t`      → list / test (read-only; work on the SFX).
//! - `upxz -c foo -o bar` → pack to a self-extractor at an explicit output path.
//!
//! The self-extractor embeds the upxz container (`magic + name + compressed
//! bytes`) after a tiny platform stub/loader, plus a trailer that records the
//! stub size so `-d`/`-l`/`-t` and the "already packed" check can locate it.
//! Running the SFX extracts the original into memory (Linux memfd) or a temp
//! file (macOS/Windows) and execs it.

mod bin_run;
mod compress;
mod format;
mod level;
mod sfx;

use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::format::{
    check_packable_input, classify, parse_header, sanitize_name, Codec, Header, Kind,
};
use crate::level::LevelArgs;

#[derive(Parser, Debug)]
#[command(
    name = "upxz",
    version,
    about = "upx using zstd — pack a file into a self-extractor that still runs.",
    after_help = "EXAMPLES:
  upxz notes.txt                       pack   → notes.txt.upxz (a self-extractor; ./notes.txt.upxz runs)
  ./notes.txt.upxz                     run    → decompress + exec the original (that is the run)
  upxz notes.txt.upxz                  refused — already packed; run it directly or use -d
  upxz -d notes.txt.upxz               unpack → restore the original (executable bit restored)
  upxz -l notes.txt.upxz               list   → codec, sizes, original name
  upxz -t notes.txt.upxz               test   → verify magic + round-trip decompress
  upxz --fast notes.txt                pack at zstd level 1 (lowest CPU, hot loops)
  upxz -z 9 notes.txt                  pack at zstd level 9 (range 1..=19)
  upxz --gz notes.txt                  pack with gzip instead of zstd
  upxz -c myapp -o myapp.sfx           pack to a self-extractor at an explicit output path
  upxz --bin bin/myapp app.tar.zst -- --flag value   run one entry from a .tar.zst

A plain FILE is packed into a runnable self-extractor <FILE>.upxz. An
already-packed file is refused (run it directly instead). Compression: default
zstd 19; --fast zstd 1; -z N zstd N (1..=19); --gz gzip (1..=9, default 9)."
)]
struct Cli {
    /// Input file. A plain file → packed to a self-extractor <FILE>.upxz
    /// (runnable via `./<FILE>.upxz`); an already-packed file → refused.
    #[arg(value_name = "FILE")]
    file: PathBuf,

    /// Run a single entry from a `.tar.zst` archive **without extracting the
    /// whole archive** (AppImage-style: archive-distributed + run inner).
    /// `--bin <inner-path>` selects the entry to run (e.g. `bin/myapp`); the
    /// `FILE` positional is the `.tar.zst` archive. The archive is streamed
    /// (zstd-decoded + tar-parsed) and only the matched entry's bytes are
    /// materialized — into a memfd on Linux, or a temp file (+ ad-hoc
    /// codesign) on macOS — then `execve`d. Trailing args after `--` are
    /// forwarded verbatim to the inner binary.
    #[arg(long = "bin", value_name = "INNER_PATH")]
    bin: Option<String>,

    /// Create a self-extracting binary (SFX). `upxz -c <orig> -o <packed>`
    /// writes an executable `packed` file that, when run, decompresses and
    /// execs the original. The original is the `FILE` positional; the output
    /// path is given by `-o`/`--out`.
    ///
    /// The SFX mechanism is platform-specific:
    /// - **Linux**: `packed = [stub][.upxz][trailer]`. The stub uses
    ///   `memfd_create` + `fexecve` so the original lives only in memory — no
    ///   temp file on disk.
    /// - **macOS**: `packed = [upxz-loader][.upxz][trailer]` (two segments).
    ///   The loader binary IS the packed file's Mach-O header (codesigned);
    ///   `./packed` execs the loader directly, which decompresses the original
    ///   to a temp file and execs it (macOS has no in-memory exec). The
    ///   appended app bytes break `codesign --verify --strict`, but exec is
    ///   unaffected (AMFI accepts the loader's cdhash).
    /// - **Windows**: `packed = [stub][.upxz][trailer]`, same shape as Linux.
    ///   The Windows stub writes the restored PE to `%TEMP%` and
    ///   `CreateProcessW`s it (Windows has no portable in-memory exec; the
    ///   NT-section route is documented in mneme `docs/upxz/windows.md` but not
    ///   compiled). No ad-hoc code-signing is needed on Windows.
    #[arg(short = 'c', long = "create-sfx")]
    create_sfx: bool,

    /// Output path for `-c` (the SFX file). Required when `-c` is set. Kept as
    /// an option (not a second positional) so that `run`/`--bin` trailing args
    /// after `--` are not swallowed by a positional slot — see `ARGS` below.
    #[arg(
        short = 'o',
        long = "out",
        value_name = "PACKED",
        requires = "create_sfx"
    )]
    sfx_output: Option<PathBuf>,

    /// Decompress / restore a .upxz container to its original file.
    #[arg(short = 'd', long = "decompress")]
    decompress: bool,

    /// List: codec / sizes / original name (read-only).
    #[arg(short = 'l', long = "list")]
    list: bool,

    /// Test: verify magic + round-trip decompress (read-only).
    #[arg(short = 't', long = "test")]
    test: bool,

    #[command(flatten)]
    level: LevelArgs,

    /// Force overwrite existing output.
    #[arg(short = 'f', long = "force")]
    force: bool,

    /// Quiet (suppress progress on stderr).
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,

    /// Verbose.
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Trailing args after `--`, forwarded verbatim to the restored binary on
    /// run. Captured with `trailing_var_arg` + `allow_hyphen_values` so
    /// `upxz foo.upxz -- -a -b` passes `-a -b` (including hyphen-leading ones)
    /// to the inner program rather than letting upxz parse them itself.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    trailing: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(inner) = &cli.bin {
        return bin_run::run(&cli.file, inner, cli.quiet, &cli.trailing);
    }
    if cli.create_sfx {
        let out = cli
            .sfx_output
            .ok_or_else(|| anyhow::anyhow!("-c requires an output path"))?;
        return pack_sfx(&cli.file, &out, &cli.level, cli.force, cli.quiet);
    }
    if cli.list {
        return list(&cli.file);
    }
    if cli.test {
        return test(&cli.file);
    }
    if cli.decompress {
        return unpack(&cli.file, cli.force, cli.quiet);
    }
    // default action: a plain file is packed into a self-extractor <FILE>.upxz;
    // an already-packed upxz artifact (bare container or SFX) is refused — run
    // it directly or restore the original with -d.
    let buf = read_input_file(&cli.file)?;
    match classify(&buf) {
        Kind::Plain => {
            let out = default_pack_output(&cli.file);
            pack_sfx(&cli.file, &out, &cli.level, cli.force, cli.quiet)
        }
        Kind::Packed { .. } => {
            // Use just the file name for the "run it directly" hint so the
            // message reads `./zhhz.upxz` regardless of whether the user typed
            // `upxz zhhz.upxz`, `upxz ./zhhz.upxz`, or an absolute path.
            let name = cli
                .file
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| cli.file.display().to_string());
            bail!(
                "{} is already a packed upxz binary.\n  run it directly:   ./{}\n  restore original:  upxz -d {}",
                cli.file.display(),
                name,
                cli.file.display(),
            )
        }
    }
}

/// Read a regular file fully. upxz is single-file-in/single-file-out, so a full
/// read is simplest and correct; directories are refused explicitly.
fn read_input_file(path: &Path) -> Result<Vec<u8>> {
    let meta =
        fs::metadata(path).with_context(|| format!("cannot stat input {}", path.display()))?;
    ensure!(
        meta.is_file(),
        "input {} is not a regular file (upxz does not handle directories)",
        path.display()
    );
    let mut f =
        fs::File::open(path).with_context(|| format!("cannot open input {}", path.display()))?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    f.read_to_end(&mut buf)
        .with_context(|| format!("cannot read input {}", path.display()))?;
    Ok(buf)
}

fn unpack(input: &Path, force: bool, quiet: bool) -> Result<()> {
    let buf = read_input_file(input)?;
    let container = packed_container_slice(&buf, input)?;
    let (header, payload_offset) = parse_header(container)?;
    let restored = compress::decompress(header.codec, &container[payload_offset..])?;

    let out_path = unpack_output(input, &header.name)?;
    if out_path.exists() && !force {
        bail!(
            "output {} already exists; use -f to overwrite",
            out_path.display()
        );
    }
    ensure!(
        !out_path.is_dir(),
        "output {} is a directory",
        out_path.display()
    );

    fs::write(&out_path, &restored)
        .with_context(|| format!("cannot write to {}", out_path.display()))?;

    // Restore the executable bit when the original looked like an executable.
    // The upxz container stores no mode bits, so a packed executable would
    // otherwise come back non-executable (the common case for upxz — "upx using
    // zstd" — is packing executables). Text and other non-executables keep the
    // default non-exec mode.
    #[cfg(unix)]
    let made_exec = if looks_executable(&restored) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&out_path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("cannot chmod output {}", out_path.display()))?;
        true
    } else {
        false
    };
    #[cfg(not(unix))]
    let made_exec = false;

    if !quiet {
        eprintln!(
            "unpacked {} -> {} ({} -> {} bytes{})",
            input.display(),
            out_path.display(),
            buf.len(),
            restored.len(),
            if made_exec { ", chmod +x" } else { "" }
        );
    }
    Ok(())
}

/// Return the embedded UPXZ container slice of an already-packed file (a bare
/// container or a self-extractor). Errors for a plain (not-yet-packed) file.
/// Shared by `-d` / `-l` / `-t` so they all locate the container the same way.
fn packed_container_slice<'a>(buf: &'a [u8], file: &Path) -> Result<&'a [u8]> {
    match classify(buf) {
        Kind::Plain => bail!("{} is not a packed upxz file", file.display()),
        Kind::Packed { offset, len } => Ok(&buf[offset..offset + len]),
    }
}

/// Heuristic: does `bytes` look like an executable whose exec bit should be
/// restored on unpack? Matches ELF, Mach-O (32/64-bit, both endians), PE/COFF
/// (`MZ`), and `#!`-shebang scripts. Non-executables (text, archives, images)
/// return false so they are not needlessly marked executable.
fn looks_executable(bytes: &[u8]) -> bool {
    const MAGIC_ELF: &[u8; 4] = b"\x7fELF";
    const MACHO_BE64: &[u8; 4] = b"\xfe\xed\xfa\xcf";
    const MACHO_LE64: &[u8; 4] = b"\xcf\xfa\xed\xfe";
    const MACHO_BE32: &[u8; 4] = b"\xfe\xed\xfa\xce";
    const MACHO_LE32: &[u8; 4] = b"\xce\xfa\xed\xfe";
    match bytes.get(..4) {
        Some(m)
            if m == MAGIC_ELF
                || m == MACHO_BE64
                || m == MACHO_LE64
                || m == MACHO_BE32
                || m == MACHO_LE32 =>
        {
            true
        }
        _ => bytes.starts_with(b"MZ") || bytes.starts_with(b"#!"),
    }
}

/// Pack an input file into a **self-extracting executable** (SFX).
///
/// The SFX layout depends on the target OS:
/// - **Linux** (`pack_sfx_linux`): `[ stub ][ .upxz container ][ trailer: u64 stub_size BE ]`.
///   The stub reads `/proc/self/exe`, slices the `.upxz` out, decompresses it
///   into a memfd, and `fexecve`s it — no temp file.
/// - **macOS** (`pack_sfx_macos`): `[ upxz-loader ][ .upxz container ][ trailer ]`.
///   Two-segment design: the packed file's Mach-O header IS the loader
///   (codesigned). `./packed` execs the loader directly; the loader reads its
///   own trailer, slices the app segment out, decompresses it to a temp file,
///   re-signs it, and `execv`s it. macOS has no in-memory exec. The appended
///   app bytes make `codesign --verify --strict` fail, but exec is unaffected
///   (AMFI accepts the loader's cdhash).
/// - **Windows** (`pack_sfx_windows`): `[ stub ][ .upxz container ][ trailer: u64 stub_size BE ]`.
///   Same shape as the Linux stub. The Windows stub (`upxz-winstub`) writes
///   the restored PE to `%TEMP%\upxz-<pid>-<tag>-<stem>.exe` and
///   `CreateProcessW`s it, then removes the temp file after the child exits.
///   Windows has no portable in-memory exec; the NT-section route is
///   documented (mneme `docs/upxz/windows.md`) but not compiled — see
///   `winstub/src/main.rs`. Windows does NOT require ad-hoc code-signing for
///   a local exec (unlike macOS AMFI), so no re-sign step.
///
/// On other targets `pack_sfx` refuses with an explicit error rather than
/// producing a non-functional artifact.
fn pack_sfx(
    input: &Path,
    output: &Path,
    level: &LevelArgs,
    force: bool,
    quiet: bool,
) -> Result<()> {
    let raw = read_input_file(input)?;
    check_packable_input(&raw)?; // refuse double-pack
    let name = sanitize_name(input)?;

    let codec = level.codec();
    let lvl = match codec {
        Codec::Zstd => level.resolve(),
        Codec::Gzip => level.gzip_level() as i32,
    };
    let payload = compress::compress(codec, raw.as_slice(), lvl)
        .with_context(|| format!("{codec} compression failed"))?;
    let header_bytes = Header {
        name: name.clone(),
        codec,
    }
    .encode();

    if output.exists() && !force {
        bail!(
            "output {} already exists; use -f to overwrite",
            output.display()
        );
    }

    // The macOS two-segment loader is no_std + zstd-sys FFI only (size gate:
    // < 1/5 of upxz, < 100 KB). It cannot decode gzip. Refuse gzip SFX on
    // macOS rather than emit a packed file the loader cannot run. The Linux
    // stub and the cross-platform runner path support gzip fully.
    #[cfg(target_os = "macos")]
    {
        if codec == Codec::Gzip {
            bail!(
                "gzip SFX is not supported on macOS: the no_std loader is zstd-only for size. \
                 Use the runner path (`upxz run foo.upxz`) or pack with zstd (drop --gz)."
            );
        }
    }

    #[cfg(target_os = "linux")]
    {
        pack_sfx_linux(
            input,
            output,
            codec,
            lvl,
            quiet,
            &raw,
            &header_bytes,
            &payload,
        )
    }
    #[cfg(target_os = "macos")]
    {
        pack_sfx_macos(
            input,
            output,
            codec,
            lvl,
            quiet,
            &raw,
            &header_bytes,
            &payload,
        )
    }
    #[cfg(target_os = "windows")]
    {
        pack_sfx_windows(
            input,
            output,
            codec,
            lvl,
            quiet,
            &raw,
            &header_bytes,
            &payload,
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (
            input,
            output,
            codec,
            lvl,
            quiet,
            &raw,
            &header_bytes,
            &payload,
        );
        bail!("upxz -c (SFX) is only supported on Linux, macOS, and Windows; rebuild upxz on the target platform");
    }
}

/// Linux SFX: `[ stub ][ .upxz container ][ trailer: u64 stub_size BE ]`.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)] // 8 args: all are distinct inputs the SFX layout needs.
fn pack_sfx_linux(
    input: &Path,
    output: &Path,
    codec: Codec,
    level: i32,
    quiet: bool,
    raw: &[u8],
    header_bytes: &[u8],
    payload: &[u8],
) -> Result<()> {
    let stub = sfx::stub_bytes().ok_or_else(|| {
        anyhow::anyhow!("upxz was not built with the Linux SFX stub; rebuild on Linux")
    })?;
    ensure!(
        !stub.is_empty(),
        "upxz-stub artifact is empty; the SFX stub did not build correctly"
    );

    let stub_size = u64::try_from(stub.len()).context("stub too large to address")?;
    let mut out = fs::File::create(output)
        .with_context(|| format!("cannot create output {}", output.display()))?;
    out.write_all(stub)
        .with_context(|| format!("cannot write stub to {}", output.display()))?;
    out.write_all(header_bytes)
        .with_context(|| format!("cannot write header to {}", output.display()))?;
    out.write_all(payload)
        .with_context(|| format!("cannot write payload to {}", output.display()))?;
    out.write_all(&stub_size.to_be_bytes())
        .with_context(|| format!("cannot write trailer to {}", output.display()))?;
    out.flush()?;

    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(output, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("cannot chmod output {}", output.display()))?;
    }

    if !quiet {
        let total = stub.len() + header_bytes.len() + payload.len() + 8;
        eprintln!(
            "sfx {} -> {} ({} -> {} bytes; stub {}, payload {}, {} {})",
            input.display(),
            output.display(),
            raw.len(),
            total,
            stub.len(),
            payload.len(),
            codec.name(),
            level
        );
    }
    Ok(())
}

/// macOS SFX (two-segment): `[ upxz-loader ][ .upxz container ][ trailer ]`.
///
/// The trailer is 16 bytes at the very end:
/// ```text
///   b"UPXZEND1"   (8 bytes magic)
///   loader_len    (u32 big-endian)   <- length of the loader Mach-O segment
///   app_len       (u32 big-endian)   <- length of the .upxz container segment
/// ```
/// The packed file's Mach-O header IS the loader (codesigned). `./packed`
/// execs the loader directly: the kernel reads the `mach_header` at offset 0,
/// AMFI accepts the loader's cdhash, and the appended app bytes are ignored at
/// exec time (they DO make `codesign --verify --strict` fail, but exec works —
/// verified empirically). The loader reads its own path via
/// `_NSGetExecutablePath`, slices the app segment out at offset `loader_len`,
/// zstd-decompresses it, writes the restored binary to `/tmp/upxz-app-<pid>`,
/// ad-hoc codesigns that, and `execv`s it. See mneme `docs/upxz/` for the full
/// design and the codesign-decision trade-off.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)] // 8 args: all are distinct inputs the SFX layout needs.
fn pack_sfx_macos(
    input: &Path,
    output: &Path,
    codec: Codec,
    level: i32,
    quiet: bool,
    raw: &[u8],
    header_bytes: &[u8],
    payload: &[u8],
) -> Result<()> {
    let loader = sfx::macos_loader_bytes().ok_or_else(|| {
        anyhow::anyhow!("upxz was not built with the macOS SFX loader; rebuild on macOS")
    })?;
    ensure!(
        !loader.is_empty(),
        "upxz-loader artifact is empty; the SFX loader did not build correctly"
    );

    // Build the trailer: magic + loader_len + app_len (all u32 BE).
    // app_len is the full .upxz container length (header + payload).
    let app_len = u32::try_from(header_bytes.len() + payload.len())
        .context(".upxz container too large to address")?;
    let loader_len = u32::try_from(loader.len()).context("loader too large to address")?;
    let trailer: Vec<u8> = {
        let mut t = Vec::with_capacity(16);
        t.extend_from_slice(b"UPXZEND1");
        t.extend_from_slice(&loader_len.to_be_bytes());
        t.extend_from_slice(&app_len.to_be_bytes());
        t
    };

    let mut out = fs::File::create(output)
        .with_context(|| format!("cannot create output {}", output.display()))?;
    out.write_all(loader)
        .with_context(|| format!("cannot write loader to {}", output.display()))?;
    out.write_all(header_bytes)
        .with_context(|| format!("cannot write header to {}", output.display()))?;
    out.write_all(payload)
        .with_context(|| format!("cannot write payload to {}", output.display()))?;
    out.write_all(&trailer)
        .with_context(|| format!("cannot write trailer to {}", output.display()))?;
    out.flush()?;

    // chmod +x so `./packed` execs the loader Mach-O. The loader is already
    // codesigned (build.rs signs it standalone), and exec does not require the
    // appended app bytes to be part of the signature.
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(output, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("cannot chmod output {}", output.display()))?;
    }

    if !quiet {
        let total = loader.len() + header_bytes.len() + payload.len() + trailer.len();
        eprintln!(
            "sfx {} -> {} ({} -> {} bytes; loader {}, payload {}, {} {})",
            input.display(),
            output.display(),
            raw.len(),
            total,
            loader.len(),
            payload.len(),
            codec.name(),
            level
        );
    }
    Ok(())
}

/// Windows SFX: `[ stub ][ .upxz container ][ trailer: u64 stub_size BE ]`.
///
/// The layout mirrors the Linux stub exactly (a single stub segment followed
/// by the `.upxz` container and an 8-byte big-endian `stub_size` trailer).
/// The Windows stub (`upxz-winstub`) resolves its own path via
/// `GetModuleFileNameW`, reads the trailer, slices the `.upxz` out,
/// decompresses it (zstd or gzip per the codec byte), writes the restored PE
/// to `%TEMP%\upxz-<pid>-<tag>-<stem>.exe`, `CreateProcessW`s it with argv
/// forwarded verbatim, and removes the temp file after the child exits. See
/// `winstub/src/main.rs` for why the NT-section in-memory route is documented
/// but not compiled.
///
/// Unlike macOS, Windows does NOT require ad-hoc code-signing for a local
/// exec, so there is no re-sign step here. (Windows Defender / SmartScreen may
/// prompt on first run of an unknown .exe — that is host-level behaviour, not
/// something upxz can or should bypass.)
#[cfg(target_os = "windows")]
#[allow(clippy::too_many_arguments)] // 8 args: all are distinct inputs the SFX layout needs.
fn pack_sfx_windows(
    input: &Path,
    output: &Path,
    codec: Codec,
    level: i32,
    quiet: bool,
    raw: &[u8],
    header_bytes: &[u8],
    payload: &[u8],
) -> Result<()> {
    let stub = sfx::windows_stub_bytes().ok_or_else(|| {
        anyhow::anyhow!("upxz was not built with the Windows SFX stub; rebuild on Windows")
    })?;
    ensure!(
        !stub.is_empty(),
        "upxz-winstub artifact is empty; the SFX stub did not build correctly"
    );

    let stub_size = u64::try_from(stub.len()).context("stub too large to address")?;
    let mut out = fs::File::create(output)
        .with_context(|| format!("cannot create output {}", output.display()))?;
    out.write_all(stub)
        .with_context(|| format!("cannot write stub to {}", output.display()))?;
    out.write_all(header_bytes)
        .with_context(|| format!("cannot write header to {}", output.display()))?;
    out.write_all(payload)
        .with_context(|| format!("cannot write payload to {}", output.display()))?;
    out.write_all(&stub_size.to_be_bytes())
        .with_context(|| format!("cannot write trailer to {}", output.display()))?;
    out.flush()?;

    // Mark the output executable. On Windows the executable bit is implicit
    // for `.exe` files, but we set it anyway so a copied `.exe` retains the
    // bit on filesystems that track it. The packed output should be named with
    // a `.exe` suffix by the caller; we do not enforce the extension here.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(output, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("cannot chmod output {}", output.display()))?;
    }

    if !quiet {
        let total = stub.len() + header_bytes.len() + payload.len() + 8;
        eprintln!(
            "sfx {} -> {} ({} -> {} bytes; stub {}, payload {}, {} {})",
            input.display(),
            output.display(),
            raw.len(),
            total,
            stub.len(),
            payload.len(),
            codec.name(),
            level
        );
    }
    Ok(())
}

fn list(file: &Path) -> Result<()> {
    let buf = read_input_file(file)?;
    let container = packed_container_slice(&buf, file)?;
    let (header, payload_offset) = parse_header(container)?;
    let original = compress::decompress(header.codec, &container[payload_offset..])
        .context("decompression failed while listing")?;
    // `container` is the embedded UPXZ container; the whole file may be larger
    // (a self-extractor adds a stub/loader + trailer). Report both so the user
    // sees the SFX overhead.
    let (kind, is_sfx) = match classify(&buf) {
        Kind::Packed { offset: 0, .. } => ("upxz container", false),
        Kind::Packed { .. } => ("upxz self-extractor", true),
        Kind::Plain => unreachable!("packed_container_slice rejected Plain"),
    };
    let ratio = if original.is_empty() {
        0.0
    } else {
        container.len() as f64 / original.len() as f64 * 100.0
    };
    println!("file\t{}", file.display());
    println!("kind\t{kind}");
    println!("codec\t{}", header.codec.name());
    println!("name\t{}", header.name);
    println!("packed\t{} bytes", container.len());
    if is_sfx {
        println!("file-size\t{} bytes", buf.len());
    }
    println!("original\t{} bytes", original.len());
    println!("ratio\t{:.1}% of original", ratio);
    Ok(())
}

fn test(file: &Path) -> Result<()> {
    let buf = read_input_file(file)?;
    let container = packed_container_slice(&buf, file)?;
    let (header, payload_offset) = parse_header(container)?;
    compress::decompress(header.codec, &container[payload_offset..])
        .context("decompression test failed")?;
    println!(
        "ok\t{} (magic valid, {} round-trip ok, original name {})",
        file.display(),
        header.codec.name(),
        header.name
    );
    Ok(())
}

/// Default pack output: `<input>.upxz`.
fn default_pack_output(input: &Path) -> PathBuf {
    let mut s = input.as_os_str().to_owned();
    s.push(".upxz");
    PathBuf::from(s)
}

/// Unpack output: strip a trailing `.upxz` from the input path; otherwise
/// restore into cwd using the (sanitized) name from the container header.
fn unpack_output(input: &Path, header_name: &str) -> Result<PathBuf> {
    let p = input.to_string_lossy();
    if let Some(stripped) = p.strip_suffix(".upxz") {
        Ok(PathBuf::from(stripped))
    } else {
        Ok(PathBuf::from(sanitize_name(Path::new(header_name))?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "upxz-test-{}-{}",
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sfx_pack_then_unpack_roundtrips() {
        // The default pack path now produces a self-extractor (not a bare
        // container). Round-trip through pack_sfx -> unpack must restore the
        // original, and the SFX must be classified as already-packed (so
        // `upxz <sfx>` refuses instead of re-packing).
        let dir = tmpdir("sfx-rt");
        let in_path = dir.join("hello.txt");
        let body = b"hello upxz\n".repeat(64);
        std::fs::write(&in_path, &body).unwrap();

        let packed = dir.join("hello.txt.upxz");
        pack_sfx(&in_path, &packed, &LevelArgs::default(), false, true).unwrap();
        assert!(packed.is_file());

        let packed_bytes = std::fs::read(&packed).unwrap();
        assert!(
            matches!(classify(&packed_bytes), Kind::Packed { .. }),
            "SFX output must classify as already-packed"
        );
        // refuse double-pack: the SFX is not a packable plain file.
        assert!(check_packable_input(&packed_bytes).is_err());

        // unpack strips .upxz -> restores hello.txt (-f overwrites the original,
        // which pack left in place).
        unpack(&packed, true, true).unwrap();
        assert_eq!(std::fs::read(dir.join("hello.txt")).unwrap(), body);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_detects_plain_container_and_sfx() {
        // plain bytes -> Plain
        assert!(matches!(classify(b"hello world"), Kind::Plain));
        // bare container (starts with the UPXZ magic) -> Packed at offset 0
        let container = Header {
            name: "x".to_owned(),
            codec: Codec::Zstd,
        }
        .encode();
        assert!(matches!(
            classify(&container),
            Kind::Packed { offset: 0, .. }
        ));
        // self-extractor -> Packed at offset > 0 (past the stub/loader)
        let dir = tmpdir("classify");
        let in_path = dir.join("in.txt");
        std::fs::write(&in_path, b"some data here").unwrap();
        let sfx = dir.join("in.txt.upxz");
        pack_sfx(&in_path, &sfx, &LevelArgs::default(), false, true).unwrap();
        let sfx_bytes = std::fs::read(&sfx).unwrap();
        assert!(matches!(
            classify(&sfx_bytes),
            Kind::Packed { offset, .. } if offset > 0
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn looks_executable_detects_common_formats() {
        assert!(looks_executable(b"\x7fELF\x02\x01\x01")); // ELF
        assert!(looks_executable(b"\xcf\xfa\xed\xfe\x07")); // Mach-O 64 LE
        assert!(looks_executable(b"\xfe\xed\xfa\xcf")); // Mach-O 64 BE
        assert!(looks_executable(b"MZ\x90\x00")); // PE/COFF
        assert!(looks_executable(b"#!/bin/sh\necho hi")); // shebang script
        assert!(!looks_executable(b"plain text file"));
        assert!(!looks_executable(b"UPXZ\x01\x00\x00")); // a container, not an exec
    }

    #[test]
    fn gzip_codec_roundtrips_at_format_level() {
        // gzip compress/decompress + the codec byte, at the format layer. (The
        // macOS SFX loader is zstd-only, so a gzip SFX cannot be built on macOS;
        // the codec round-trip itself is platform-independent.)
        let body = b"hello upxz gzip\n".repeat(64);
        let payload = compress::compress(Codec::Gzip, &body, 9).unwrap();
        let restored = compress::decompress(Codec::Gzip, &payload).unwrap();
        assert_eq!(restored, body);
        let header = Header {
            name: "x".to_owned(),
            codec: Codec::Gzip,
        }
        .encode();
        assert_eq!(&header[..5], b"UPXZ\x01");
        assert_eq!(header[5], 1, "gzip codec byte must be 1");
    }

    #[test]
    fn parse_header_reads_gzip_codec() {
        // A header written with codec=Gzip must parse back as codec=Gzip.
        let h = Header {
            name: "x.bin".to_owned(),
            codec: Codec::Gzip,
        };
        let bytes = h.encode();
        let (parsed, off) = parse_header(&bytes).unwrap();
        assert_eq!(parsed.codec, Codec::Gzip);
        assert_eq!(parsed.name, "x.bin");
        assert_eq!(off, bytes.len()); // payload starts right after the header
    }

    #[test]
    fn level_resolves_per_priority() {
        assert_eq!(LevelArgs::default().resolve(), 19);
    }

    #[test]
    fn directories_are_rejected() {
        let dir = tmpdir("notfile");
        let err = read_input_file(&dir).unwrap_err();
        assert!(format!("{err}").contains("not a regular file"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
