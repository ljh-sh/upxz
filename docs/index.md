---
layout: default
title: Home
---

# upxz

**upx using zstd** — pack a file into a self-extracting binary that still runs.

```bash
upxz notes.txt               # pack   → notes.txt.upxz (self-extractor; chmod +x)
./notes.txt.upxz             # run    → decompress + exec the original (that IS the run)
upxz notes.txt.upxz          # refused — already packed; run it directly or use -d
upxz -d notes.txt.upxz       # unpack → restore the original (chmod +x for executables)
```

`upxz <FILE>` produces a **self-extracting executable** (`<FILE>.upxz`) — `./<FILE>.upxz` runs the original directly. The CLI has **no run mode** inside upxz; the SFX runs itself.

## Quick example

```bash
x eget ljh-sh/zhhz           # fetch a binary
upxz ./zhhz                  # ./zhhz.upxz is a self-extractor (chmod +x)
./zhhz.upxz --version        # runs the original (zhhz 0.7.7)
upxz -d ./zhhz.upxz          # restore ./zhhz (chmod +x restored)
```

## Features

- **Self-extractor by default.** `upxz FILE` → runnable SFX (chmod +x), `./FILE.upxz` runs the original.
- **musl-static Linux.** One binary runs on Alpine **and** every glibc distro (Ubuntu / Debian / Fedora / Arch).
- **Cross-arch.** 5 release targets: `x86_64-linux-musl`, `aarch64-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.
- **Codec-agnostic container** (zstd default, gzip via `--gz`).
- **`-d` restores the executable bit** when the original was an executable.
- **OpenSSF suite**: scorecard + codeql + dependabot.
- **No runtime.** Static single binary; statically links zstd.

## Install

Prebuilt binary (cosign-signed, with `SHA256SUMS`):

```bash
# pick the tarball for your platform, then:
tar xJf upxz-<target>.tar.xz -C /usr/local/bin --strip-components=1 bin/upxz

# verify
sha256sum -c SHA256SUMS --ignore-missing
cosign verify-blob --bundle upxz-<target>.tar.xz.sigstore.json upxz-<target>.tar.xz
```

See [Install](/install/) for all 5 platforms and building from source.

## Learn more

- [Install](/install/) — every platform, verification, building from source
- [CLI](/cli/) — full command reference
- [Format](/format/) — container and SFX on-disk format
- [GitHub repo](https://github.com/ljh-sh/upxz) · [Releases](https://github.com/ljh-sh/upxz/releases) · [CHANGELOG](https://github.com/ljh-sh/upxz/blob/main/CHANGELOG.md)

## License

Apache-2.0. The `zstd` bindings are MIT.
