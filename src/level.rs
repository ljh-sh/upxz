//! Compression level for upxz (mneme#41 final design).
//!
//! 2-tier preset + explicit override. zstd only. No `--best`, never -22.
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

use clap::Args;

pub const ZSTD_DEFAULT: i32 = 19;
pub const ZSTD_FAST: i32 = 1;
const ZSTD_MIN: u8 = 1;
const ZSTD_MAX: u8 = 19;

/// Flattened into the top-level `Cli` (no subcommands). `--fast` and `-z N`
/// may both be present; `-z N` wins per the priority rule.
#[derive(Args, Debug, Default, Clone)]
pub struct LevelArgs {
    /// Use the fast preset (zstd level 1). Lowest CPU, best for hot loops.
    #[arg(long)]
    pub fast: bool,

    /// Explicit zstd level (1..=19). Highest priority — overrides --fast and default.
    #[arg(short = 'z', long = "zst-level", value_name = "N", value_parser = clap::value_parser!(u8).range(ZSTD_MIN as i64..=ZSTD_MAX as i64))]
    pub zst_level: Option<u8>,
}

impl LevelArgs {
    /// Resolve flags into a concrete libzstd level, honoring priority.
    pub fn resolve(&self) -> i32 {
        if let Some(n) = self.zst_level {
            n as i32
        } else if self.fast {
            ZSTD_FAST
        } else {
            ZSTD_DEFAULT
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
        };
        assert_eq!(a.resolve(), 1);
    }

    #[test]
    fn z_overrides_fast_and_default() {
        let a = LevelArgs {
            fast: true,
            zst_level: Some(7),
        };
        assert_eq!(a.resolve(), 7); // -z wins over --fast
        let b = LevelArgs {
            fast: false,
            zst_level: Some(1),
        };
        assert_eq!(b.resolve(), 1);
    }

    #[test]
    fn resolved_levels_stay_in_range() {
        for n in 1..=19u8 {
            let a = LevelArgs {
                fast: false,
                zst_level: Some(n),
            };
            let lvl = a.resolve();
            assert!((1..=19).contains(&lvl));
        }
    }
}
