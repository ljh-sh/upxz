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
        bail!("output {} already exists; use -f to overwrite", out_path.display());
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
        bail!("output {} already exists; use -f to overwrite", out_path.display());
    }
    ensure!(!out_path.is_dir(), "output {} is a directory", out_path.display());

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
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o500)).with_context(|| {
            format!("cannot chmod restored binary {}", tmp.display())
        })?;
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

fn list(file: &Path) -> Result<()> {
    let buf = read_input_file(file)?;
    let (header, payload_offset) = parse_header(&buf)?;
    let original = zstd::decode_all(&buf[payload_offset..])
        .context("decompression failed while listing")?;
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
