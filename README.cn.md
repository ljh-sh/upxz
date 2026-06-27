# upxz

[![CII Best Practices](https://bestpractices.coreinfrastructure.org/projects/0/badge)](https://bestpractices.coreinfrastructure.org/projects/0)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/ljh-sh/upxz/badge)](https://scorecard.dev/viewer/?uri=github.com/ljh-sh/upxz)

> 极简单二进制文件打包器：一个文件进、一个文件出，校验文件头 magic，用 zstd 打包。

> Tiny single-binary file packer. One file in, one file out, magic-checked, zstd-packed. — [English](README.md)

## TL;DR（速览）

```bash
upxz notes.txt               # 打包   → notes.txt.upxz（zstd，默认 level 19）
upxz notes.txt.upxz          # 运行   → 解压并执行原始文件
upxz -d notes.txt.upxz       # 解包   → 还原原始字节
upxz -l notes.txt.upxz       # 列信息 → codec / 体积 / 原始文件名
upxz -t notes.txt.upxz       # 测试   → 校验 magic + 往返解压
upxz -c myapp -o myapp.sfx   # 生成自解压二进制（./myapp.sfx 直接跑）
upxz --fast big.bin          # zstd level 1 打包（最省 CPU）
upxz --gz notes.txt          # 用 gzip 而非 zstd 打包
```

普通 `FILE` → **打包**；`.upxz` 容器 → **运行**。模式由 magic 自动判定，无需告诉 upxz。

## 这是什么

`upxz` 把单个文件包进一个小容器，再用 [zstd](https://datatracker.ietf.org/doc/html/rfc8878) 压缩。范围故意收得很窄：

- **一个文件进，一个文件出。** 没有目录、没有通配符、没有批量模式。
- **校验文件头 magic。** 容器以 8 字节 magic（`UPXZ\x01…`）开头；对已经打包过的容器再次打包会被拒绝。
- **单二进制。** 静态链接 zstd，不需要 Python，没有额外运行时。

upx 风格的**扁平 CLI**（无子命令）——用 flag 选动作，打包 vs 运行由输入 magic 自动判定：

| 用法                            | 动作                                                          |
| ------------------------------- | ------------------------------------------------------------- |
| `upxz <FILE>`                   | **打包** → `<FILE>.upxz`（magic + 原始文件名 + zstd 负载）    |
| `upxz <FILE>.upxz`              | **运行** → 解压并执行原始文件（透传退出码）                   |
| `upxz -d <FILE>.upxz`           | **解包** → 还原原始字节到磁盘                                 |
| `upxz -l <FILE>.upxz`           | **列信息** → codec / 体积 / 原始文件名（只读）                |
| `upxz -t <FILE>.upxz`           | **测试** → 校验 magic + 往返解压（只读）                      |
| `upxz -c <orig> -o <packed>`    | **SFX** → 自解压二进制                                        |
| `upxz --bin <inner> <a.tar.zst>`| **bin 运行** → 不解包整个 `.tar.zst`，只跑其中指定条目        |

## 安装

**预编译二进制**（Linux x86_64、macOS arm64、Windows x86_64 —— cosign 签名，附 `SHA256SUMS`），见 [最新 release](https://github.com/ljh-sh/upxz/releases/latest)：

```bash
# 选你平台的 tarball，然后：
tar xJf upxz-<target>.tar.xz -C /usr/local/bin --strip-components=1 bin/upxz

# 校验哈希 + 签名
sha256sum -c SHA256SUMS --ignore-missing
cosign verify-blob --bundle upxz-<target>.tar.xz.sigstore.json upxz-<target>.tar.xz
```

**从源码**（完整功能，含 SFX `-c`）：

```bash
cargo install --git https://github.com/ljh-sh/upxz
```

> **`cargo install upxz`（来自 crates.io）只有 runner/packer，没有 SFX。**
> `cargo publish` 的 tarball 会丢弃内嵌的 SFX 配套 crate（`stub/`/`loader/`/`winstub/`），
> 所以从 crates.io 装的版本无法编译自解压功能（`-c`/`--create-sfx`）—— `upxz -c` 会
> 报错提示。打包 / 运行 / `--bin` / list / test 都正常。要 SFX，请用 release 二进制
> 或 `cargo install --git`（两者都带完整源码）。见 [#11](https://github.com/ljh-sh/upxz/issues/11)。

## 用法

```bash
# 打包（zstd，默认 level 19）—— notes.txt 是普通文件，自动判定为打包
upxz notes.txt                   # -> notes.txt.upxz

# 运行容器 —— magic 表明它是打包过的，自动判定为运行
upxz notes.txt.upxz              # 解压并执行原始文件；透传退出码
upxz notes.txt.upxz -- --flag value   # -- 之后的参数原样转给原始程序

# 查看 / 校验 / 还原（多为只读）
upxz -l notes.txt.upxz           # 列信息：codec / 体积 / 原始文件名
upxz -t notes.txt.upxz           # 测试：magic + 往返解压
upxz -d notes.txt.upxz           # 解包：还原原始字节（-> notes.txt）
upxz -d notes.txt.upxz -f        # 覆盖已存在的 notes.txt

# 选压缩级别
upxz --fast notes.txt            # zstd level 1 —— 最省 CPU，高频调用
upxz -z 9 notes.txt              # zstd level 9（任意 1..=19）
upxz --gz notes.txt              # 用 gzip 而非 zstd（magic 里 codec id 为 1）

# 生成自解压二进制
upxz -c myapp -o myapp.sfx && ./myapp.sfx --flag value
```

### 压缩

两档预设 + 显式级别 —— 没有 `--best`，绝不用 `-22`（已知陷阱：体积和 `-19` 一样，
却慢约 2 倍）。`-z N` 优先级高于 `--fast`：

| 选择       | 参数      | zstd level | 适用                      |
| ---------- | --------- | ---------- | ------------------------- |
| default    | （无）    | 19         | 常见情况（体积最小）       |
| fast       | `--fast`  | 1          | 最省 CPU                  |
| explicit   | `-z N`    | N (1..=19) | 指定级别                  |

`--gz` 切到 gzip codec（DEFLATE 1..=9，默认 9）；此时 `-z N` 设的是 DEFLATE 级别。

## 为什么只用 zstd（不要 xz / liblzma）

`upxz` 不依赖 `xz2` / `liblzma`。容器只用 zstd 压缩，这样构建产物仍是
单二进制，许可证也是宽松路线（项目 Apache-2.0 + zstd 绑定为 BSD），不碰
LGPL。完整理由见 [`DESIGN.md`](DESIGN.md)。

## 容器格式

```
+------------------+----------------------+--------------------------+
| magic (8 字节)   | name-len (4 字节 BE) | 原始文件名 (UTF-8)        |
+------------------+----------------------+--------------------------+
+--------------------------------------------------------------------+
| zstd frame（原始文件的压缩字节）                                   |
+--------------------------------------------------------------------+
```

存储的文件名只是扁平的文件名组件（不含分隔符，不含 `..`）；解包时会再次
校验，所以被篡改的容器无法逃出当前目录。

## 许可证

Apache-2.0。zstd 绑定（`zstd` crate）为 MIT。
