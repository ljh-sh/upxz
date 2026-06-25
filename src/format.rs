//! on-disk format and magic-byte handling for upxz.
//!
//! A `.upxz` file is a small header followed by a zstd-compressed payload:
//!
//! ```text
//! +------------------+----------------------+--------------------------+
//! | magic (8 bytes)  | name-len (4 bytes BE)| original file name (UTF-8)|
//! +------------------+----------------------+--------------------------+
//! +--------------------------------------------------------------------+
//! | zstd frame (compressed original file bytes)                        |
//! +--------------------------------------------------------------------+
//! ```
//!
//! The container itself is intentionally tiny and dependency-free to parse so
//! that the single binary stays auditable. Compression is delegated to libzstd
//! via the `zstd` crate (BSD-licensed bindings).

use anyhow::{bail, ensure, Result};
use std::path::Path;

/// Magic prefix for every upxz container. ASCII `UPXZ\x01\x00\x00\x00`.
/// The trailing version byte leaves room to evolve the format without
/// re-purposing the prefix.
pub const MAGIC: [u8; 8] = *b"UPXZ\x01\x00\x00\x00";

/// Maximum length we will store for the original file name. Generous for any
/// realistic path, small enough that a corrupted length field cannot make us
/// allocate gigabytes before we read the payload.
pub const MAX_NAME_LEN: usize = 4096;

/// A parsed container header. Payload starts immediately after `name`.
#[derive(Debug, Clone)]
pub struct Header {
    pub name: String,
}

impl Header {
    /// Serialize the header into a fresh `Vec<u8>` (caller appends payload).
    pub fn encode(&self) -> Vec<u8> {
        let name_bytes = self.name.as_bytes();
        let name_len = u32::try_from(name_bytes.len()).unwrap_or(0);
        let mut out = Vec::with_capacity(MAGIC.len() + 4 + name_bytes.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&name_len.to_be_bytes());
        out.extend_from_slice(name_bytes);
        out
    }
}

/// Read and validate the magic + name from a byte slice that starts at offset 0
/// of a candidate container. Returns the header and the offset where the
/// compressed payload begins.
pub fn parse_header(buf: &[u8]) -> Result<(Header, usize)> {
    ensure!(
        buf.len() >= MAGIC.len() + 4,
        "input too small to be a upxz container"
    );
    ensure!(
        buf[..MAGIC.len()] == MAGIC,
        "bad magic: not a upxz container"
    );
    let name_len = u32::from_be_bytes([
        buf[MAGIC.len()],
        buf[MAGIC.len() + 1],
        buf[MAGIC.len() + 2],
        buf[MAGIC.len() + 3],
    ]) as usize;
    ensure!(
        name_len <= MAX_NAME_LEN,
        "declared name length {name_len} exceeds maximum {MAX_NAME_LEN}"
    );
    let name_start = MAGIC.len() + 4;
    let payload_start = name_start + name_len;
    ensure!(
        buf.len() >= payload_start,
        "truncated container: header declares {name_len} name bytes"
    );
    let name = std::str::from_utf8(&buf[name_start..payload_start])
        .map_err(|e| anyhow::anyhow!("original file name is not valid UTF-8: {e}"))?
        .to_owned();
    Ok((Header { name }, payload_start))
}

/// Returns true if `bytes` begins with the upxz magic. Used to refuse
/// double-packing an already-packed file.
pub fn has_magic(bytes: &[u8]) -> bool {
    bytes.len() >= MAGIC.len() && bytes[..MAGIC.len()] == MAGIC
}

/// Extract the final path component of a path as a String, refusing empty
/// names, path separators, and parent-directory segments. This keeps the
/// stored name flat — upxz has no concept of directories, so a malicious or
/// garbled input cannot smuggle a path traversal into the unpack step.
pub fn sanitize_name(path: &Path) -> Result<String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("input path has no file name component"))?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("input file name is not valid UTF-8"))?;
    ensure!(!file_name.is_empty(), "file name is empty");
    ensure!(
        !file_name.contains('/') && !file_name.contains('\\'),
        "file name must not contain a path separator"
    );
    ensure!(
        file_name != "." && file_name != "..",
        "file name must not be a directory entry"
    );
    Ok(file_name.to_owned())
}

/// Validate that raw input bytes look like something we should pack. Today we
/// only refuse already-packed upxz containers; any other byte stream is fair
/// game because the container records the original magic implicitly via the
/// payload.
pub fn check_packable_input(bytes: &[u8]) -> Result<()> {
    if has_magic(bytes) {
        bail!("input is already a upxz container; refusing to double-pack");
    }
    Ok(())
}
