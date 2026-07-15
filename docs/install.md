---
layout: default
title: Install
---

# Install

## Prebuilt binary (recommended)

Pick the v0.4.0 release for your platform from the
[GitHub releases page](https://github.com/ljh-sh/upxz/releases/tag/v0.4.0):

| Platform | Asset |
| --- | --- |
| Linux x86_64 (musl) | `upxz-x86_64-unknown-linux-musl.tar.xz` |
| Linux aarch64 (musl) | `upxz-aarch64-unknown-linux-musl.tar.xz` |
| macOS x86_64 | `upxz-x86_64-apple-darwin.tar.xz` |
| macOS aarch64 (Apple Silicon) | `upxz-aarch64-apple-darwin.tar.xz` |
| Windows x86_64 | `upxz-x86_64-pc-windows-msvc.tar.xz` |

All Linux builds are **musl, statically linked** — the same binary runs on
Alpine and every glibc distro (Ubuntu / Debian / Fedora / Arch). No `libc`
dependency.

```bash
tar xJf upxz-<target>.tar.xz -C /usr/local/bin --strip-components=1 bin/upxz
```

### Verify the download

Each release ships `SHA256SUMS`, `SHA256SUMS.sigstore.json`, and a per-tarball
`.sigstore.json` (keyless cosign signature).

```bash
# checksum
sha256sum -c SHA256SUMS --ignore-missing

# signature (keyless cosign over an OIDC token)
cosign verify-blob --bundle upxz-<target>.tar.xz.sigstore.json upxz-<target>.tar.xz
```

`cosign verify-blob` prints `Verified OK` on success.

## Build from source

```bash
# from git (full feature set: packer + SFX -c):
cargo install --git https://github.com/ljh-sh/upxz
```

This builds with the SFX companion crates (`stub` / `loader` / `winstub`) and
includes the `upxz -c` self-extractor builder.

### From crates.io (packer only)

```bash
cargo install upxz
```

A `cargo publish` tarball strips the nested SFX companion crates, so a
crates.io install has the runner/packer and `-d`/`-l`/`-t`/`--bin` for
existing SFXes, but `upxz <FILE>` (SFX pack) refuses with a clear message.
For SFX packing, use a release binary or `cargo install --git`.

## Verify the build

```bash
# run the 45-test suite
cargo test
```
