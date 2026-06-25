# Contributing to upxz

Thanks for your interest! upxz is a small, focused tool. Please read this
short guide before opening an issue or PR.

## Scope

upxz is a **single-binary, single-file packer**:

- 1 file in, 1 file out.
- Magic-check the header, compress with zstd, or error.

Out of scope: directories, globs, batch processing, multiple codecs. If your
idea needs any of those, it probably belongs in a different tool — open an
issue first so we can talk about it.

## Reporting issues

Open a [GitHub issue](../../issues) and include:

- Operating system and architecture
- upxz version (`upxz --version`)
- Installation method (cargo / binary / source)
- The exact command you ran and a minimal input file
- Expected vs actual output

## Building from source

```bash
cargo build --release
# binary at target/release/upxz
cargo test
cargo clippy --all-targets
```

## Pull requests

- Keep the build a single binary. Do not introduce dependencies that require
  dynamic linking or non-permissive licenses.
- Run `cargo fmt`, `cargo clippy --all-targets`, and `cargo test` before
  pushing.
- Do not use auto-close keywords (`Closes`, `Fixes`, `Resolves`) in commit
  messages or PR descriptions. Link issues by number in prose instead.
