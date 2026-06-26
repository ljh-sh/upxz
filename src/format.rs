//! on-disk format and magic-byte handling for upxz.
//!
//! A `.upxz` file is a small header followed by a compressed payload:
//!
//! ```text
//! +----------------------------------+----------------------+--------------------------+
//! | magic (8 bytes, embeds codec id) | name-len (4 bytes BE)| original file name (UTF-8)|
//! +----------------------------------+----------------------+--------------------------+
//! +--------------------------------------------------------------------+
//! | compressed payload (zstd or gzip, per the codec id in the magic)  |
//! +--------------------------------------------------------------------+
//! ```
//!
//! The container itself is intentionally tiny and dependency-free to parse so
//! that the single binary stays auditable.
//!
//! ## Magic layout (8 bytes)
//!
//! ```text
//!   offset 0..4: "UPXZ"            (ASCII tag)
//!   offset 4:    0x01              (format version)
//!   offset 5:    codec id          (0 = zstd, 1 = gzip; this is the dispatch key)
//!   offset 6..8: 0x00 0x00         (reserved, must be 0 today)
//! ```
//!
//! The container is **codec-agnostic**: a single byte in the fixed-width magic
//! identifies the payload codec, so a future reader can pick the right
//! decompressor without a sidecar or probing. This keeps the format extensible
//! without changing the magic length (8 bytes is a nice round power-of-two).
//! See mneme `story/feature/260626.upxz-tech/` §codec-agnostic.

use anyhow::{bail, ensure, Result};
use std::fmt;
use std::path::Path;

/// Magic prefix length. All containers and SFX stubs agree on 8 bytes.
pub const MAGIC_LEN: usize = 8;

/// Fixed 5-byte prefix shared by every container: ASCII `UPXZ` + version `0x01`.
/// The remaining 3 bytes (`[codec, 0x00, 0x00]`) vary per container, so callers
/// must use [`codec_magic`] / [`has_magic`] instead of comparing against a
/// single literal.
pub const MAGIC_PREFIX: [u8; 5] = *b"UPXZ\x01";

/// Byte index inside the 8-byte magic that carries the codec id.
pub const CODEC_OFFSET: usize = 5;

/// The magic as it appeared in v0.1/v0.2 (codec byte implicitly `0` = zstd).
/// Kept so existing zstd containers and tests stay byte-identical, and so the
/// default pack path produces a backward-compatible file.
pub const MAGIC: [u8; 8] = *b"UPXZ\x01\x00\x00\x00";

/// A payload codec. The numeric id is what is written into the magic byte at
/// [`CODEC_OFFSET`]; the variant is the Rust-side dispatch key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    /// zstd (the original v0.1 codec). id `0`. Default + backward-compatible.
    Zstd = 0,
    /// gzip (DEFLATE + gzip wrapper). id `1`. Useful for tooling that already
    /// speaks gzip (e.g. shipping to a CDN/browser, or piping through `gunzip`).
    Gzip = 1,
}

impl Codec {
    /// Decode a codec id byte from the magic. Unknown ids (anything other than
    /// the defined variants) are rejected so a future id does not silently
    /// masquerade as zstd.
    pub fn from_id(id: u8) -> Result<Self> {
        match id {
            0 => Ok(Codec::Zstd),
            1 => Ok(Codec::Gzip),
            other => bail!("unknown upxz codec id {other} (only 0=zstd, 1=gzip are defined)"),
        }
    }

    /// Human-readable name for `--list` / `--test` output.
    pub fn name(self) -> &'static str {
        match self {
            Codec::Zstd => "zstd",
            Codec::Gzip => "gzip",
        }
    }

    /// Decode a codec id from the byte at [`CODEC_OFFSET`] of an 8-byte magic.
    /// Caller is responsible for ensuring `magic` is exactly 8 bytes.
    pub fn from_magic_byte(magic: &[u8]) -> Result<Self> {
        ensure!(magic.len() == MAGIC_LEN, "magic must be 8 bytes");
        Self::from_id(magic[CODEC_OFFSET])
    }
}

impl fmt::Display for Codec {
    /// Same surface as [`Codec::name`] — lets `format!("{codec}")` work in
    /// error/log strings without a separate helper.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Build the 8-byte magic for a given codec. The codec id is written at
/// [`CODEC_OFFSET`]; the two trailing bytes stay `0x00` (reserved).
pub fn codec_magic(codec: Codec) -> [u8; MAGIC_LEN] {
    let mut m = MAGIC; // start from the zstd magic (all-zeros trailing)
    m[CODEC_OFFSET] = codec as u8;
    m
}

/// Maximum length we will store for the original file name. Generous for any
/// realistic path, small enough that a corrupted length field cannot make us
/// allocate gigabytes before we read the payload.
pub const MAX_NAME_LEN: usize = 4096;

/// A parsed container header. Payload starts immediately after `name`.
#[derive(Debug, Clone)]
pub struct Header {
    pub name: String,
    pub codec: Codec,
}

impl Header {
    /// Serialize the header into a fresh `Vec<u8>` (caller appends payload).
    /// The codec is embedded in the magic so the unpacker knows which
    /// decompressor to run.
    pub fn encode(&self) -> Vec<u8> {
        encode_with_codec(&self.name, self.codec)
    }
}

/// Build a header byte vector for `name` + `codec`. Public so the SFX packers
/// can construct one without going through `Header` (they pass name + codec
/// directly and append the payload they already compressed).
pub fn encode_with_codec(name: &str, codec: Codec) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let name_len = u32::try_from(name_bytes.len()).unwrap_or(0);
    let magic = codec_magic(codec);
    let mut out = Vec::with_capacity(MAGIC_LEN + 4 + name_bytes.len());
    out.extend_from_slice(&magic);
    out.extend_from_slice(&name_len.to_be_bytes());
    out.extend_from_slice(name_bytes);
    out
}

/// Read and validate the magic + name from a byte slice that starts at offset 0
/// of a candidate container. Returns the header (including the codec id parsed
/// out of the magic) and the offset where the compressed payload begins.
pub fn parse_header(buf: &[u8]) -> Result<(Header, usize)> {
    ensure!(
        buf.len() >= MAGIC_LEN + 4,
        "input too small to be a upxz container"
    );
    // Match the fixed 5-byte prefix (UPXZ\x01) and require the two reserved
    // trailing bytes to be zero. The codec byte at CODEC_OFFSET is decoded
    // separately below; the prefix check guarantees the file is plausibly ours
    // before we trust any of the variable fields.
    ensure!(
        buf[..MAGIC_PREFIX.len()] == MAGIC_PREFIX,
        "bad magic: not a upxz container"
    );
    ensure!(
        buf[MAGIC_PREFIX.len() + 1] == 0 && buf[MAGIC_PREFIX.len() + 2] == 0,
        "bad magic: reserved bytes are non-zero (not a known upxz format)"
    );
    let codec = Codec::from_magic_byte(&buf[..MAGIC_LEN])?;
    let name_len = u32::from_be_bytes([
        buf[MAGIC_LEN],
        buf[MAGIC_LEN + 1],
        buf[MAGIC_LEN + 2],
        buf[MAGIC_LEN + 3],
    ]) as usize;
    ensure!(
        name_len <= MAX_NAME_LEN,
        "declared name length {name_len} exceeds maximum {MAX_NAME_LEN}"
    );
    let name_start = MAGIC_LEN + 4;
    let payload_start = name_start + name_len;
    ensure!(
        buf.len() >= payload_start,
        "truncated container: header declares {name_len} name bytes"
    );
    let name = std::str::from_utf8(&buf[name_start..payload_start])
        .map_err(|e| anyhow::anyhow!("original file name is not valid UTF-8: {e}"))?
        .to_owned();
    Ok((Header { name, codec }, payload_start))
}

/// Returns true if `bytes` begins with a valid upxz magic of any known codec.
/// Accepts the v0.1/v0.2 zstd magic AND the gzip magic; rejects unknown codec
/// ids and non-zero reserved bytes. Used to refuse double-packing an
/// already-packed file and to auto-detect pack-vs-run.
pub fn has_magic(bytes: &[u8]) -> bool {
    if bytes.len() < MAGIC_LEN {
        return false;
    }
    if bytes[..MAGIC_PREFIX.len()] != MAGIC_PREFIX {
        return false;
    }
    // Reserved bytes must be zero. A future format that reuses them will bump
    // the version byte, so we never want to false-positive on it here.
    if bytes[MAGIC_PREFIX.len() + 1] != 0 || bytes[MAGIC_PREFIX.len() + 2] != 0 {
        return false;
    }
    Codec::from_magic_byte(&bytes[..MAGIC_LEN]).is_ok()
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
