//! Compression level selection.
//!
//! Decision (ljh-sh/mneme#41, Decision 2 = option A): upxz exposes zstd
//! compression as a 3-tier named preset rather than a raw `--level=N` knob.
//! The tiers map onto libzstd's 1..=22 range:
//!
//! | tier      | flag       | zstd level | intent                              |
//! |-----------|------------|------------|-------------------------------------|
//! | fast      | `--fast`   | 1          | minimize CPU, for hot loops         |
//! | default   | _(none)_   | 3          | good ratio at low cost (zstd's own) |
//! | best      | `--best`   | 19         | pay CPU for smallest output         |
//!
//! Rationale: a single-binary packer aimed at scripts and AI agents should not
//! force the caller to internalize libzstd's level numbers on every invocation.
//! A nameless default covers the common case, `--fast` is self-explanatory for
//! latency-sensitive callers, and `--best` signals "I will trade CPU for
//! bytes" without ambiguity. A 2-tier split omits the comfortable middle that
//! most users actually want; a raw numeric flag shifts the decision back onto
//! the caller with no sensible default. Three tiers is the smallest set that
//! gives a clear default plus both escape hatches.

use clap::Args;

/// Compression preset chosen on the command line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Level {
    /// `--fast`: zstd level 1.
    Fast,
    /// _(default)_: zstd level 3.
    #[default]
    Default,
    /// `--best`: zstd level 19.
    Best,
}

impl Level {
    /// Map the preset onto a concrete libzstd level (1..=22).
    pub fn zstd_level(self) -> i32 {
        match self {
            Level::Fast => 1,
            Level::Default => 3,
            Level::Best => 19,
        }
    }
}

/// Clap-flattened flags. Exactly one of `--fast` / `--best` may be set; if
/// neither is given the default tier applies. Conflicts are declared so clap
/// rejects `--fast --best` with a clear error rather than silently preferring
/// one.
#[derive(Args, Debug, Default)]
pub struct LevelArgs {
    /// Use the fast preset (zstd level 1). Lowest CPU.
    #[arg(long, conflicts_with = "best")]
    pub fast: bool,

    /// Use the best preset (zstd level 19). Smallest output, highest CPU.
    #[arg(long, conflicts_with = "fast")]
    pub best: bool,
}

impl LevelArgs {
    /// Resolve the flags into a single `Level`. Calling this is only valid on
    /// parsed args (clap enforces mutual exclusion via `conflicts_with`).
    pub fn resolve(&self) -> Level {
        if self.fast {
            Level::Fast
        } else if self.best {
            Level::Best
        } else {
            Level::Default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tier_is_level_3() {
        assert_eq!(LevelArgs::default().resolve(), Level::Default);
        assert_eq!(Level::Default.zstd_level(), 3);
    }

    #[test]
    fn fast_and_best_map_to_expected_zstd_levels() {
        assert_eq!(Level::Fast.zstd_level(), 1);
        assert_eq!(Level::Best.zstd_level(), 19);
    }

    #[test]
    fn all_levels_stay_inside_libzstd_range() {
        for lvl in [Level::Fast, Level::Default, Level::Best] {
            let n = lvl.zstd_level();
            assert!((1..=22).contains(&n), "{lvl:?} -> {n} out of range");
        }
    }
}
