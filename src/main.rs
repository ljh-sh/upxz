//! upxz — tiny single-binary file packer.
//!
//! Scope (ljh-sh/mneme#41): upxz takes exactly one file in and writes exactly
//! one file out. It magic-checks the header, packs with zstd, or errors. There
//! is no concept of directories, globs, or batch processing — that complexity
//! belongs in a different tool.
//!
//! Two subcommands:
//! - `pack <INPUT> [-o OUTPUT] [--fast|--best]` — wrap one file in a `.upxz`
//!   container (magic + original name + zstd-compressed bytes).
//! - `unpack <INPUT> [-o OUTPUT]`           — reverse: verify magic, restore
//!   the original bytes to disk.
//!
//! Decision 1 (mneme#41): xz2 is intentionally **not** a dependency. The
//! container compresses with zstd only, so the build stays a single static
//! binary with a permissive (Apache-2.0 + BSD/MIT) license story and no
//! liblzma / LGPL entanglement.

mod format;
mod level;

use anyhow::{ensure, Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::format::{check_packable_input, parse_header, sanitize_name, Header};
use crate::level::LevelArgs;

#[derive(Parser, Debug)]
#[command(
    name = "upxz",
    version,
    about = "Tiny single-binary file packer. One file in, one file out."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Wrap one file in a .upxz container (magic + name + zstd payload).
    Pack {
        /// Input file. Must be a real file, not a directory.
        input: PathBuf,
        /// Output path. Defaults to `<input>.upxz`.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        level: LevelArgs,
    },
    /// Restore a .upxz container back to its original bytes.
    Unpack {
        /// Input .upxz container.
        input: PathBuf,
        /// Output path. Defaults to the name stored in the container header,
        /// written into the current directory.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Pack {
            input,
            output,
            level,
        } => pack(&input, output.as_deref(), level.resolve()),
        Cmd::Unpack { input, output } => unpack(&input, output.as_deref()),
    }
}

/// Read a regular file fully into memory. upxz is a single-file-in/single-file-
/// out tool, so a full read is the simple and correct approach; we refuse
/// directories explicitly to keep the "no directory concepts" contract crisp.
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

fn pack(input: &Path, output: Option<&Path>, level: level::Level) -> Result<()> {
    let raw = read_input_file(input)?;
    check_packable_input(&raw)?;

    let name = sanitize_name(input)?;
    let header = Header { name };

    let zstd_level = level.zstd_level();
    let payload = zstd::encode_all(raw.as_slice(), zstd_level)
        .with_context(|| format!("zstd compression failed at level {zstd_level}"))?;

    let out_path = output
        .map(|p| p.to_owned())
        .unwrap_or_else(|| default_pack_output(input));

    let header_bytes = header.encode();
    let mut out = fs::File::create(&out_path)
        .with_context(|| format!("cannot create output {}", out_path.display()))?;
    out.write_all(&header_bytes)
        .with_context(|| format!("cannot write header to {}", out_path.display()))?;
    out.write_all(&payload)
        .with_context(|| format!("cannot write payload to {}", out_path.display()))?;
    out.flush()?;

    eprintln!(
        "packed {} -> {} ({} -> {} bytes, zstd level {})",
        input.display(),
        out_path.display(),
        raw.len(),
        header_bytes.len() + payload.len(),
        zstd_level
    );
    Ok(())
}

fn unpack(input: &Path, output: Option<&Path>) -> Result<()> {
    let buf = read_input_file(input)?;
    let (header, payload_offset) = parse_header(&buf)?;
    let compressed = &buf[payload_offset..];

    let restored = zstd::decode_all(compressed)
        .context("zstd decompression failed; container may be corrupt")?;

    let out_path = match output {
        Some(p) => p.to_owned(),
        None => {
            // Restore into the current directory using the stored name. We
            // re-validate the name defensively so a tampered container cannot
            // escape the cwd via a crafted header.
            let safe = sanitize_name(Path::new(&header.name)).context(
                "container stores an unsafe file name; use -o to choose an explicit output",
            )?;
            PathBuf::from(safe)
        }
    };

    ensure!(
        !out_path.is_dir(),
        "output path {} is a directory",
        out_path.display()
    );

    let mut out = fs::File::create(&out_path)
        .with_context(|| format!("cannot create output {}", out_path.display()))?;
    out.write_all(&restored)
        .with_context(|| format!("cannot write to {}", out_path.display()))?;
    out.flush()?;

    eprintln!(
        "unpacked {} -> {} ({} compressed -> {} bytes, name from header)",
        input.display(),
        out_path.display(),
        buf.len(),
        restored.len()
    );
    Ok(())
}

/// Default pack output is `<input>.upxz`, preserving the original extension in
/// the stored name (the `.upxz` suffix is the *container* extension, not part
/// of the original file name recorded in the header).
fn default_pack_output(input: &Path) -> PathBuf {
    let mut s = input.as_os_str().to_owned();
    s.push(".upxz");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_then_unpack_roundtrips_a_small_file() {
        let dir = std::env::temp_dir().join(format!(
            "upxz-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let in_path = dir.join("hello.txt");
        let body = b"hello upxz\n".repeat(64);
        std::fs::write(&in_path, &body).unwrap();

        let packed = dir.join("hello.txt.upxz");
        pack(&in_path, Some(&packed), level::Level::Default).unwrap();
        assert!(packed.is_file());

        // packed file must start with the magic.
        let packed_bytes = std::fs::read(&packed).unwrap();
        assert_eq!(&packed_bytes[..format::MAGIC.len()], &format::MAGIC);

        // refusing double-pack.
        assert!(pack(&packed, None, level::Level::Default).is_err());

        let restored = dir.join("restored.txt");
        unpack(&packed, Some(&restored)).unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), body);

        // default unpack path uses the header name.
        let cwd_restore = dir.join("hello.txt"); // header stored "hello.txt"
                                                 // run an unpack with no -o from inside `dir` so the restored name lands there.
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        // remove the original so the round-trip is unambiguous
        let _ = std::fs::remove_file("hello.txt");
        unpack(&packed, None).unwrap();
        std::env::set_current_dir(prev).unwrap();
        assert_eq!(std::fs::read(cwd_restore).unwrap(), body);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fast_and_best_both_produce_valid_containers() {
        let dir = std::env::temp_dir().join(format!(
            "upxz-lvl-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("data.bin");
        let body = vec![0x41u8; 8192];
        std::fs::write(&in_path, &body).unwrap();

        for lvl in [level::Level::Fast, level::Level::Best] {
            let packed = dir.join(format!("data.{}.upxz", lvl.zstd_level()));
            pack(&in_path, Some(&packed), lvl).unwrap();
            let restored = dir.join(format!("data.{}.out", lvl.zstd_level()));
            unpack(&packed, Some(&restored)).unwrap();
            assert_eq!(std::fs::read(&restored).unwrap(), body);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn directories_are_rejected() {
        let dir = std::env::temp_dir().join("upxz-not-a-file");
        std::fs::create_dir_all(&dir).unwrap();
        let err = read_input_file(&dir).unwrap_err();
        assert!(format!("{err}").contains("not a regular file"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
