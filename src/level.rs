//! Compression level + codec selection for upxz (mneme#41 final design).
//!
//! **Level** (zstd only — gzip uses its own DEFLATE level map below):
//!
//! 2-tier preset + explicit override. No `--best`, never -22.
//!
//! | selection | flag              | zstd level |
//! |-----------|-------------------|------------|
//! | default   | _(none)_          | 19         |
//! | fast      | `--fast`          | 1          |
//! | explicit  | `-z N` (1..=19)   | N          |
//!
//! Priority (high → low): `-z N` > `--fast` > default.
//! `-z N` covers default and --fast; range capped at 19 (20..=22 need libzstd
//! `--ultra` and -22 is a documented trap: same size as -19, 2x slower comp).
//!
//! **Codec** (`--gz`): the v0.3 container is codec-agnostic. The codec id is
//! embedded in the magic; see [`crate::format::Codec`]. `--gz` selects gzip
//! (codec id 1); the absence of the flag leaves the default zstd (codec id 0),
//! which is also what every pre-0.3 container uses. The pack and SFX paths
//! both honor `--gz`.

use clap::Args;

use crate::format::Codec;

pub const ZSTD_DEFAULT: i32 = 19;
pub const ZSTD_FAST: i32 = 1;
const ZSTD_MIN: u8 = 1;
const ZSTD_MAX: u8 = 19;

/// DEFLATE level used when `--gz` is set and no explicit `-z` is given.
/// flate2 accepts 1..=9 (with 0=store, 6=zlib default). We pick 9 for parity
/// with the zstd default (smallest output); `-z N` overrides it if the caller
/// wants a faster gzip.
pub const GZIP_DEFAULT: u32 = 9;

/// Flattened into the top-level `Cli` (no subcommands). `--fast` and `-z N`
/// may both be present; `-z N` wins per the priority rule. `--gz` selects the
/// gzip codec and is independent of the level flags.
#[derive(Args, Debug, Default, Clone)]
pub struct LevelArgs {
    /// Use the fast preset (zstd level 1). Lowest CPU, best for hot loops.
    #[arg(long)]
    pub fast: bool,

    /// Explicit zstd level (1..=19). Highest priority — overrides --fast and default.
    #[arg(short = 'z', long = "zst-level", value_name = "N", value_parser = clap::value_parser!(u8).range(ZSTD_MIN as i64..=ZSTD_MAX as i64))]
    pub zst_level: Option<u8>,

    /// Compress with **gzip** instead of zstd. Writes codec id `1` into the
    /// container magic. `-z N` (1..=9) sets the DEFLATE level; without `-z` it
    /// defaults to 9 (best compression, matching the zstd default philosophy).
    /// Existing zstd containers are still read transparently — this only
    /// affects what gets written on a new pack.
    #[arg(long = "gz")]
    pub gzip: bool,
}

impl LevelArgs {
    /// Resolve flags into a concrete libzstd level, honoring priority. Only
    /// meaningful when the codec is zstd.
    pub fn resolve(&self) -> i32 {
        if let Some(n) = self.zst_level {
            n as i32
        } else if self.fast {
            ZSTD_FAST
        } else {
            ZSTD_DEFAULT
        }
    }

    /// The codec to pack with, derived from `--gz`.
    pub fn codec(&self) -> Codec {
        if self.gzip {
            Codec::Gzip
        } else {
            Codec::Zstd
        }
    }

    /// DEFLATE level for the gzip codec. `-z N` (clamped to 1..=9) overrides
    /// the default of 9; `--fast` maps to 1 for parity with the zstd preset.
    pub fn gzip_level(&self) -> u32 {
        if let Some(n) = self.zst_level {
            n.clamp(1, 9) as u32
        } else if self.fast {
            1
        } else {
            GZIP_DEFAULT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_19() {
        assert_eq!(LevelArgs::default().resolve(), 19);
        assert_eq!(ZSTD_DEFAULT, 19);
    }

    #[test]
    fn fast_is_1() {
        let a = LevelArgs {
            fast: true,
            zst_level: None,
            gzip: false,
        };
        assert_eq!(a.resolve(), 1);
    }

    #[test]
    fn z_overrides_fast_and_default() {
        let a = LevelArgs {
            fast: true,
            zst_level: Some(7),
            gzip: false,
        };
        assert_eq!(a.resolve(), 7); // -z wins over --fast
        let b = LevelArgs {
            fast: false,
            zst_level: Some(1),
            gzip: false,
        };
        assert_eq!(b.resolve(), 1);
    }

    #[test]
    fn resolved_levels_stay_in_range() {
        for n in 1..=19u8 {
            let a = LevelArgs {
                fast: false,
                zst_level: Some(n),
                gzip: false,
            };
            let lvl = a.resolve();
            assert!((1..=19).contains(&lvl));
        }
    }

    #[test]
    fn default_codec_is_zstd() {
        assert_eq!(LevelArgs::default().codec(), Codec::Zstd);
    }

    #[test]
    fn gz_flag_selects_gzip() {
        let a = LevelArgs {
            gzip: true,
            ..LevelArgs::default()
        };
        assert_eq!(a.codec(), Codec::Gzip);
    }

    #[test]
    fn gzip_default_level_is_9() {
        let a = LevelArgs {
            gzip: true,
            ..LevelArgs::default()
        };
        assert_eq!(a.gzip_level(), 9);
    }

    #[test]
    fn gzip_level_clamps_z_to_1_9() {
        // -z 19 on gzip clamps to 9 (the DEFLATE ceiling).
        let a = LevelArgs {
            gzip: true,
            zst_level: Some(19),
            ..LevelArgs::default()
        };
        assert_eq!(a.gzip_level(), 9);
        let b = LevelArgs {
            gzip: true,
            zst_level: Some(3),
            ..LevelArgs::default()
        };
        assert_eq!(b.gzip_level(), 3);
    }

    #[test]
    fn gzip_fast_is_level_1() {
        let a = LevelArgs {
            gzip: true,
            fast: true,
            ..LevelArgs::default()
        };
        assert_eq!(a.gzip_level(), 1);
    }
}
