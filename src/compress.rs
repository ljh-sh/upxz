//! Codec dispatch: compress / decompress a byte payload by [`Codec`].
//!
//! The container is codec-agnostic (the codec id lives in the magic; see
//! [`crate::format::Codec`]). Every read path (unpack / run / list / test) and
//! every write path (pack / SFX) funnels through here, so adding a third codec
//! is a single new variant + two `match` arms.
//!
//! Backends:
//! - **zstd** (`Codec::Zstd`): the `zstd` crate (libzstd via zstd-sys). Levels
//!   1..=19, default 19.
//! - **gzip** (`Codec::Gzip`): the `flate2` crate with the pure-Rust
//!   `miniz_oxide` backend (no C `libz` dependency, no system linkage). DEFLATE
//!   levels 1..=9, default 9.
//!
//! `miniz_oxide` is chosen over `zlib-ng` so the upxz binary stays
//! statically-linked and dependency-light (the same reason we ship zstd-sys
//! rather than a system libzstd). It is marginally slower than zlib-ng at
//! compression but identical at decompression, which is the hot SFX path.

use anyhow::{Context, Result};
use std::io::{Read, Write};

use crate::format::Codec;

/// Compress `raw` into a fresh `Vec<u8>` using `codec` at `level`.
///
/// `level` is codec-specific:
/// - zstd: 1..=19 (we cap at 19; 20..=22 need `--ultra` and -22 is a trap).
/// - gzip: 1..=9 (0 = store / no compression is rejected by callers).
pub fn compress(codec: Codec, raw: &[u8], level: i32) -> Result<Vec<u8>> {
    match codec {
        Codec::Zstd => zstd::encode_all(raw, level)
            .with_context(|| format!("zstd compression failed at level {level}")),
        Codec::Gzip => {
            // flate2::GzEncoder takes a Compression(1..=9). Clamp defensively.
            let lvl = level.clamp(1, 9) as u32;
            let mut enc = flate2::write::GzEncoder::new(
                Vec::with_capacity(raw.len() / 2),
                flate2::Compression::new(lvl),
            );
            enc.write_all(raw)
                .with_context(|| format!("gzip compression failed at level {lvl}"))?;
            enc.finish()
                .with_context(|| format!("gzip flush failed at level {lvl}"))
        }
    }
}

/// Decompress a full payload buffer into a fresh `Vec<u8>`. `payload` must be
/// exactly the compressed bytes (header already stripped by the caller).
pub fn decompress(codec: Codec, payload: &[u8]) -> Result<Vec<u8>> {
    match codec {
        Codec::Zstd => {
            zstd::decode_all(payload).context("zstd decompression failed; container may be corrupt")
        }
        Codec::Gzip => {
            let mut dec = flate2::read::GzDecoder::new(payload);
            let mut out = Vec::new();
            dec.read_to_end(&mut out)
                .context("gzip decompression failed; container may be corrupt")?;
            Ok(out)
        }
    }
}
