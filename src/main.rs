//! upxz — upx using zstd. A **runner** + packer (mneme#41 final design).
//!
//! A `.upxz` file is a container: `magic + original-name + zstd(original bytes)`.
//! - `upxz foo`        → if foo is a plain file: **pack** to `foo.upxz`
//! - `upxz foo.upxz`   → magic detected: **run** (decompress original to a temp
//!                       file and exec it, propagating exit code)
//! - `upxz -d foo.upxz`→ **unpack** (restore original bytes)
//! - `upxz -l/-t`      → list / test (read-only)
//!
//! Design choice: upxz does **not** rewrite binaries in place and injects no
//! loader stub. The runner is an ordinary signed binary on each OS; the
//! restored original execs independently with its own signature. That avoids
//! all Mach-O/ELF fixup, codesign and PAC entanglement — so it works on macOS,
//! Linux, and (later) Windows with one container format.

mod format;
mod level;
mod sfx;

use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::format::{check_packable_input, has_magic, parse_header, sanitize_name, Header, MAGIC};
use crate::level::LevelArgs;

#[derive(Parser, Debug)]
#[command(
    name = "upxz",
    version,
    about = "upx using zstd — runner + packer. Auto-detects pack vs run by magic."
)]
struct Cli {
    /// Input file. A plain file → packed to <FILE>.upxz; a .upxz container → run.
    #[arg(value_name = "FILE")]
    file: PathBuf,

    /// Create a self-extracting binary (SFX). `upxz -c <orig> <packed>` writes
    /// an executable `packed` file that, when run, decompresses and execs the
    /// original. The original is the first positional, the output is the
    /// second.
    ///
    /// The SFX mechanism is platform-specific:
    /// - **Linux**: `packed = [stub][.upxz][trailer]`. The stub uses
    ///   `memfd_create` + `fexecve` so the original lives only in memory — no
    ///   temp file on disk.
    /// - **macOS**: `packed = [boot sh][upxz-loader][.upxz][trailer]` (three
    ///   segments). The boot script (`#!/bin/sh`) extracts the embedded
    ///   `upxz-loader` to a cache dir and execs it; the loader decompresses
    ///   the original to a temp file and execs that (macOS has no in-memory
    ///   exec). The packed file itself is a shell script and does not need to
    ///   be codesigned.
    #[arg(short = 'c', long = "create-sfx")]
    create_sfx: bool,

    /// When used with `-c`, the output SFX path. Required iff `-c` is set.
    #[arg(value_name = "PACKED", requires = "create_sfx")]
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
    if cli.create_sfx {
        let out = cli
            .sfx_output
            .ok_or_else(|| anyhow::anyhow!("-c requires an output path"))?;
        return pack_sfx(&cli.file, &out, cli.level.resolve(), cli.force, cli.quiet);
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
    // default action: auto-detect by magic
    let head = read_head(&cli.file, MAGIC.len())?;
    if has_magic(&head) {
        run(&cli.file, cli.quiet, &cli.trailing)
    } else {
        pack(&cli.file, cli.level.resolve(), cli.force, cli.quiet)
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

/// Read at most `n` leading bytes for magic sniffing.
fn read_head(path: &Path, n: usize) -> Result<Vec<u8>> {
    let mut f =
        fs::File::open(path).with_context(|| format!("cannot open input {}", path.display()))?;
    let mut head = vec![0u8; n];
    let got = f.read(&mut head).unwrap_or(0);
    head.truncate(got);
    Ok(head)
}

fn pack(input: &Path, zstd_level: i32, force: bool, quiet: bool) -> Result<()> {
    let raw = read_input_file(input)?;
    check_packable_input(&raw)?; // refuse double-pack

    let name = sanitize_name(input)?;
    let payload = zstd::encode_all(raw.as_slice(), zstd_level)
        .with_context(|| format!("zstd compression failed at level {zstd_level}"))?;

    let out_path = default_pack_output(input);
    if out_path.exists() && !force {
        bail!(
            "output {} already exists; use -f to overwrite",
            out_path.display()
        );
    }
    let header_bytes = Header { name }.encode();
    let mut out = fs::File::create(&out_path)
        .with_context(|| format!("cannot create output {}", out_path.display()))?;
    out.write_all(&header_bytes)
        .with_context(|| format!("cannot write header to {}", out_path.display()))?;
    out.write_all(&payload)
        .with_context(|| format!("cannot write payload to {}", out_path.display()))?;
    out.flush()?;

    if !quiet {
        eprintln!(
            "packed {} -> {} ({} -> {} bytes, zstd {})",
            input.display(),
            out_path.display(),
            raw.len(),
            header_bytes.len() + payload.len(),
            zstd_level
        );
    }
    Ok(())
}

fn unpack(input: &Path, force: bool, quiet: bool) -> Result<()> {
    let buf = read_input_file(input)?;
    let (header, payload_offset) = parse_header(&buf)?;
    let restored = zstd::decode_all(&buf[payload_offset..])
        .context("zstd decompression failed; container may be corrupt")?;

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

    if !quiet {
        eprintln!(
            "unpacked {} -> {} ({} -> {} bytes)",
            input.display(),
            out_path.display(),
            buf.len(),
            restored.len()
        );
    }
    Ok(())
}

/// Run a `.upxz` container: decompress the original to a temp file and exec it,
/// propagating the child's exit code. Trailing args (after `--`) are forwarded
/// verbatim to the restored binary. The temp file is removed after the child
/// exits.
///
/// On macOS a restored copy of a signed Mach-O no longer matches its original
/// signature, so AMFI kills it with SIGKILL (exit 137) on exec. We re-sign the
/// temp copy ad-hoc (`codesign --sign - --force`) so the kernel accepts it.
/// Linux needs no signing.
fn run(file: &Path, quiet: bool, trailing: &[String]) -> Result<()> {
    let buf = read_input_file(file)?;
    let (header, payload_offset) = parse_header(&buf)?;
    let original = zstd::decode_all(&buf[payload_offset..])
        .context("zstd decompression failed; container may be corrupt")?;

    let tmp = std::env::temp_dir().join(format!(
        ".upxz-run-{}-{}",
        std::process::id(),
        sanitize_tmp_name(&header.name)
    ));
    fs::write(&tmp, &original)
        .with_context(|| format!("cannot write restored binary to {}", tmp.display()))?;
    #[cfg(target_os = "macos")]
    {
        // Ad-hoc re-sign the temp copy: macOS AMFI SIGKILLs (exit 137) a copied
        // signed binary on exec otherwise. `--force` overwrites the stale
        // signature. A non-zero status is fatal — without a valid signature the
        // child cannot run at all. Done before the chmod to 0o500 below, because
        // codesign must write the new signature to the file.
        let cs = std::process::Command::new("codesign")
            .args(["--sign", "-", "--force", "--"])
            .arg(&tmp)
            .output()
            .with_context(|| "failed to spawn codesign")?;
        if !cs.status.success() {
            bail!(
                "codesign failed on {} (exit {:?}): {}",
                tmp.display(),
                cs.status.code(),
                String::from_utf8_lossy(&cs.stderr).trim()
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o500))
            .with_context(|| format!("cannot chmod restored binary {}", tmp.display()))?;
    }

    if !quiet {
        eprintln!(
            "run {} ({} bytes restored, exec {})",
            file.display(),
            original.len(),
            header.name
        );
    }

    let status = std::process::Command::new(&tmp)
        .args(trailing)
        .status()
        .with_context(|| format!("failed to exec restored binary {}", tmp.display()))?;
    let _ = fs::remove_file(&tmp);
    std::process::exit(status.code().unwrap_or(1));
}

/// Pack an input file into a **self-extracting executable** (SFX).
///
/// The SFX layout depends on the target OS:
/// - **Linux** (`pack_sfx_linux`): `[ stub ][ .upxz container ][ trailer: u64 stub_size BE ]`.
///   The stub reads `/proc/self/exe`, slices the `.upxz` out, decompresses it
///   into a memfd, and `fexecve`s it — no temp file.
/// - **macOS** (`pack_sfx_macos`): `[ boot sh ][ upxz-loader ][ .upxz ][ trailer ]`.
///   `./packed` runs the boot script, which extracts the loader to a cache dir
///   and execs it; the loader decompresses the original to a temp file and
///   execs that. macOS has no in-memory exec. The packed file is a shell
///   script and is not codesigned.
///
/// On other targets `pack_sfx` refuses with an explicit error rather than
/// producing a non-functional artifact.
fn pack_sfx(input: &Path, output: &Path, zstd_level: i32, force: bool, quiet: bool) -> Result<()> {
    let raw = read_input_file(input)?;
    check_packable_input(&raw)?; // refuse double-pack
    let name = sanitize_name(input)?;

    let payload = zstd::encode_all(raw.as_slice(), zstd_level)
        .with_context(|| format!("zstd compression failed at level {zstd_level}"))?;
    let header_bytes = Header { name: name.clone() }.encode();

    if output.exists() && !force {
        bail!(
            "output {} already exists; use -f to overwrite",
            output.display()
        );
    }

    #[cfg(target_os = "linux")]
    {
        pack_sfx_linux(
            input,
            output,
            zstd_level,
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
            zstd_level,
            quiet,
            &raw,
            &header_bytes,
            &payload,
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (
            input,
            output,
            zstd_level,
            quiet,
            &raw,
            &header_bytes,
            &payload,
        );
        bail!("upxz -c (SFX) is only supported on Linux and macOS; rebuild upxz on the target platform");
    }
}

/// Linux SFX: `[ stub ][ .upxz container ][ trailer: u64 stub_size BE ]`.
#[cfg(target_os = "linux")]
fn pack_sfx_linux(
    input: &Path,
    output: &Path,
    zstd_level: i32,
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
            "sfx {} -> {} ({} -> {} bytes; stub {}, payload {}, zstd {})",
            input.display(),
            output.display(),
            raw.len(),
            total,
            stub.len(),
            payload.len(),
            zstd_level
        );
    }
    Ok(())
}

/// macOS SFX: `[ boot sh ][ upxz-loader ][ .upxz container ][ trailer ]`.
///
/// The trailer is 20 bytes at the very end:
/// ```text
///   b"UPXZEND1"   (8 bytes magic)
///   boot_len      (u32 big-endian)
///   loader_len    (u32 big-endian)
///   app_len       (u32 big-endian)   <- length of the .upxz container
/// ```
/// `./packed` runs the boot script (the file\'s shebang makes the kernel exec
/// `/bin/sh` on it); the trailing binary bytes after the script are ignored by
/// sh. Boot extracts the loader segment to `~/.cache/upxz/upxz-loader-<len>`,
/// ad-hoc codesigns it, and execs it with `argv[1]=packed`. The loader reads
/// the same trailer, locates the app segment, zstd-decompresses it, writes the
/// restored binary to `/tmp/upxz-app-<pid>`, ad-hoc codesigns that, and
/// `execv`s it. See mneme `docs/upxz/` for the full design.
#[cfg(target_os = "macos")]
fn pack_sfx_macos(
    input: &Path,
    output: &Path,
    zstd_level: i32,
    quiet: bool,
    raw: &[u8],
    header_bytes: &[u8],
    payload: &[u8],
) -> Result<()> {
    let boot = sfx::macos_boot_bytes().ok_or_else(|| {
        anyhow::anyhow!("upxz was not built with the macOS SFX boot script; rebuild on macOS")
    })?;
    let loader = sfx::macos_loader_bytes().ok_or_else(|| {
        anyhow::anyhow!("upxz was not built with the macOS SFX loader; rebuild on macOS")
    })?;
    ensure!(
        !boot.is_empty(),
        "upxz boot script artifact is empty; the SFX boot did not build correctly"
    );
    ensure!(
        !loader.is_empty(),
        "upxz-loader artifact is empty; the SFX loader did not build correctly"
    );

    // Build the trailer: magic + boot_len + loader_len + app_len (all u32 BE).
    // app_len is the full .upxz container length (header + payload).
    let app_len = u32::try_from(header_bytes.len() + payload.len())
        .context(".upxz container too large to address")?;
    let boot_len = u32::try_from(boot.len()).context("boot script too large to address")?;
    let loader_len = u32::try_from(loader.len()).context("loader too large to address")?;
    let trailer: Vec<u8> = {
        let mut t = Vec::with_capacity(20);
        t.extend_from_slice(b"UPXZEND1");
        t.extend_from_slice(&boot_len.to_be_bytes());
        t.extend_from_slice(&loader_len.to_be_bytes());
        t.extend_from_slice(&app_len.to_be_bytes());
        t
    };

    let mut out = fs::File::create(output)
        .with_context(|| format!("cannot create output {}", output.display()))?;
    out.write_all(boot)
        .with_context(|| format!("cannot write boot script to {}", output.display()))?;
    out.write_all(loader)
        .with_context(|| format!("cannot write loader to {}", output.display()))?;
    out.write_all(header_bytes)
        .with_context(|| format!("cannot write header to {}", output.display()))?;
    out.write_all(payload)
        .with_context(|| format!("cannot write payload to {}", output.display()))?;
    out.write_all(&trailer)
        .with_context(|| format!("cannot write trailer to {}", output.display()))?;
    out.flush()?;

    // chmod +x so `./packed` runs the boot shebang.
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(output, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("cannot chmod output {}", output.display()))?;
    }

    if !quiet {
        let total = boot.len() + loader.len() + header_bytes.len() + payload.len() + trailer.len();
        eprintln!(
            "sfx {} -> {} ({} -> {} bytes; boot {}, loader {}, payload {}, zstd {})",
            input.display(),
            output.display(),
            raw.len(),
            total,
            boot.len(),
            loader.len(),
            payload.len(),
            zstd_level
        );
    }
    Ok(())
}

fn list(file: &Path) -> Result<()> {
    let buf = read_input_file(file)?;
    let (header, payload_offset) = parse_header(&buf)?;
    let original =
        zstd::decode_all(&buf[payload_offset..]).context("decompression failed while listing")?;
    let ratio = if original.is_empty() {
        0.0
    } else {
        buf.len() as f64 / original.len() as f64 * 100.0
    };
    println!("file\t{}", file.display());
    println!("magic\tUPXZ (upxz container)");
    println!("codec\tzstd");
    println!("name\t{}", header.name);
    println!("compressed\t{} bytes", buf.len());
    println!("original\t{} bytes", original.len());
    println!("ratio\t{:.1}% of original", ratio);
    Ok(())
}

fn test(file: &Path) -> Result<()> {
    let buf = read_input_file(file)?;
    let (header, payload_offset) = parse_header(&buf)?;
    zstd::decode_all(&buf[payload_offset..]).context("decompression test failed")?;
    println!(
        "ok\t{} (magic valid, zstd round-trip ok, original name {})",
        file.display(),
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

/// Flatten an arbitrary stored name into something safe to embed in a temp path.
fn sanitize_tmp_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
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
    fn pack_then_unpack_roundtrips() {
        let dir = tmpdir("rt");
        let in_path = dir.join("hello.txt");
        let body = b"hello upxz\n".repeat(64);
        std::fs::write(&in_path, &body).unwrap();

        let packed = dir.join("hello.txt.upxz");
        pack(&in_path, 19, false, true).unwrap();
        assert!(packed.is_file());
        let packed_bytes = std::fs::read(&packed).unwrap();
        assert_eq!(&packed_bytes[..MAGIC.len()], &MAGIC[..]);

        // refuse double-pack
        assert!(pack(&packed, 19, false, true).is_err());

        // unpack default strips .upxz -> restores to "hello.txt" next to it.
        // pack keeps the original, so it still exists and must be overwritten.
        unpack(&packed, true, true).unwrap();
        assert_eq!(std::fs::read(dir.join("hello.txt")).unwrap(), body);

        std::fs::remove_dir_all(&dir).ok();
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
