# upxz design notes

This records the two design decisions resolved in `ljh-sh/mneme#41` and the
constraints that drove them.

## Constraints

upxz is a **single-binary** file packer:

- 1 file in, 1 file out.
- Magic-check the header.
- Pack, or error. There is no concept of directories, globs, or batch
  processing — that complexity belongs in a different tool.

The build must continue to produce a single binary.

## Decision 1 — compression backend: drop xz2 (option A)

**Choice:** drop `xz2` / liblzma entirely. The container compresses with zstd
only.

**Why not option B (keep xz2, accept LGPL):**
- `xz2` binds to liblzma, which is LGPL. For a tool whose whole value prop is
  "one self-contained binary", an LGPL dependency either forces dynamic linking
  (breaking the single-binary story) or imposes source-offering obligations
  that raise the bar for redistribution.
- It also pulls in a C library build, widening the supply chain.

**Why not option C (pure-Rust xz):**
- A packer rarely benefits from a second compression codec layered on top of
  pre-compressed input. Adding a pure-Rust LZMA implementation grows the
  dependency tree and binary for marginal gain.
- zstd is modern, fast, and BSD-licensed; it is the right default.

**Result:** `Cargo.toml` depends on `zstd` only. The release build is a single
statically-linked binary (no `xz2`, no `liblzma`).

## Decision 2 — zstd compression level: 3-tier preset (option A)

**Choice:** expose zstd as three named tiers.

| tier    | flag     | zstd level |
| ------- | -------- | ---------- |
| default | _(none)_ | 3          |
| fast    | `--fast` | 1          |
| best    | `--best` | 19         |

`--fast` and `--best` are mutually exclusive (enforced by clap
`conflicts_with`).

**Why not option B (2-tier fast / best):**
- 2-tier drops the "comfortable middle". Most callers want a sensible default
  without thinking; forcing them to pick fast-or-best on every invocation adds
  friction for the common case.

**Why not option C (raw `--level=N`):**
- A raw numeric flag shifts the decision onto the caller with no useful
  default. Callers would have to internalize libzstd's 1..=22 range.
- For a tool aimed at scripts and AI agents, a nameless default plus two
  self-explanatory escape hatches (`--fast`, `--best`) is easier to reach for
  and harder to misuse.

**Mapping rationale.** zstd's own default is 3; level 1 is the documented
"fast" end; 19 is the top of the standard range (20..22 require
`--ultra`-style opt-in and are rarely worth the cost for a general-purpose
packer). Keeping the mapping inside the standard range means we never have to
explain "why did the ratio jump oddly at level 20".

## Non-goals

- upxz does **not** support directories, archives of multiple files, or batch
  processing. Inputs that are not regular files are rejected.
- upxz does **not** try to be a general-purpose compressor. The container is a
  thin wrapper; if you only want raw zstd, use `zstd` directly.

## Decision 3 — Linux SFX stub: memfd_create + fexecve (Plan D)

**Choice:** on Linux, `upxz -c <orig> <packed>` emits a self-extracting
executable = `[ stub ELF ][ .upxz container ][ trailer u64 stub_size BE ]`.
Running `packed` makes the stub read `/proc/self/exe`, slice out the `.upxz`
container, decompress the original into a memfd, and `fexecve` it. No temp
file is written to disk.

**Why Plan D (self-contained stub, in-memory exec) over the alternatives:**

- *Plan A — keep the runner, decompress to a temp file.* This is what upxz
  already does. It works everywhere, but writes the original (which may be
  large, sensitive, or subject to integrity checks) to `/tmp` where it can be
  inspected, raced, or left behind on a crash. For a "ship one binary" story
  it is a leak.
- *Plan B — rewrite the ELF in place (UPX-style).* Requires a custom ELF
  builder, fixup of program headers, and re-signing. Massively more complex,
  fragile across kernel/glibc versions, and breaks signature verification.
  Not worth it for a zstd packer.
- *Plan C — `/proc/self/fd/N` exec after writing a real file.* Half-measure:
  still writes to disk, gains nothing over Plan A.

Plan D's stub is tiny, dependency-light (`libc` + `zstd`), and uses only
kernel primitives (`memfd_create`, `fexecve`) available since Linux 3.17 /
glibc 2.27. The trailer-records-stub-size scheme means the stub never needs
to know its own length at compile time — it discovers it from the file.

**Why Linux only:** `memfd_create` + `fexecve` are Linux-specific. macOS has
no memfd equivalent (its closest, `shm_open`, is not sealable and is backed
by `/tmp`); Windows has an entirely different exec model. The runner path
(Plan A) remains the cross-platform default. SFX is gated behind
`#[cfg(target_os = "linux")]` and `upxz -c` refuses with a clear message on
other platforms.

**How the stub gets into the packer:** `build.rs` compiles the `stub/` crate
as a release artifact and `include_bytes!`s it into `upxz` at build time, so
`upxz -c` is self-contained — no separate stub file to ship.
