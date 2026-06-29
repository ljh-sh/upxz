# upxz

[![CII Best Practices](https://bestpractices.coreinfrastructure.org/projects/0/badge)](https://bestpractices.coreinfrastructure.org/projects/0)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/ljh-sh/upxz/badge)](https://scorecard.dev/viewer/?uri=github.com/ljh-sh/upxz)

> 极简单二进制文件打包器：一个文件进、一个文件出，校验文件头 magic，用 zstd 打包。

> Tiny single-binary file packer. One file in, one file out, magic-checked, zstd-packed. — [English](README.md)

## TL;DR（速览）

```bash
upxz notes.txt               # 打包   → notes.txt.upxz（自解压二进制；./notes.txt.upxz 直接跑）
./notes.txt.upxz             # 运行   → 解压并执行原始文件（这才是真正的“运行”）
upxz notes.txt.upxz          # 拒绝   —— 已加壳，请直接跑它或用 -d 还原
upxz -d notes.txt.upxz       # 解包   → 还原原始字节（可执行文件会自动 chmod +x）
upxz -l notes.txt.upxz       # 列信息 → codec / 体积 / 原始文件名
upxz -t notes.txt.upxz       # 测试   → 校验 magic + 往返解压
upxz -c myapp -o myapp.sfx   # 打包到指定输出路径（仍为自解压二进制）
upxz --fast big.bin          # zstd level 1 打包（最省 CPU）
upxz --gz notes.txt          # 用 gzip 而非 zstd 打包
```

普通 `FILE` → **打包**为自解压二进制 `<FILE>.upxz`（直接 `./<FILE>.upxz` 运行）。
已加壳的文件 → **拒绝**（SFX 自己跑；upxz 故意没有 `upxz run` 子命令）。
读取路径（`-d`/`-l`/`-t`）都能自动定位嵌入式 UPXZ 容器（v0.1–v0.3 的纯容器和自解压二进制共用同一条读取路径）。

## 这是什么

`upxz` 把单个文件包进一个小容器，再用 [zstd](https://datatracker.ietf.org/doc/html/rfc8878) 压缩。范围故意收得很窄：

- **一个文件进，一个文件出。** 没有目录、没有通配符、没有批量模式。
- **upx 风格的产物。** 默认打包输出**自解压可执行文件**（`<FILE>.upxz`，`chmod +x`）。`./<FILE>.upxz` 直接执行原文件 —— upxz 内部没有单独的“运行”步骤。
- **校验文件头 magic。** 对已加壳文件（纯容器或自解压）再次打包会被拒绝。
- **单二进制。** 静态链接 zstd，不需要 Python，没有额外运行时。

upx 风格的**扁平 CLI**（无子命令）——用 flag 选动作：

| 用法                            | 动作                                                          |
| ------------------------------- | ------------------------------------------------------------- |
| `upxz <FILE>`                   | **打包** → `<FILE>.upxz`（自解压：stub + 容器 + trailer，`chmod +x`） |
| `./<FILE>.upxz`                 | **运行** → 解压并执行原始文件（这才是真正的“运行”；透传退出码）    |
| `upxz <FILE>.upxz`              | **拒绝** —— 已加壳；请直接跑 SFX 或用 `-d` 还原                |
| `upxz -d <FILE>.upxz`           | **解包** → 还原原始字节到磁盘（可执行文件自动恢复 `chmod +x`）   |
| `upxz -l <FILE>.upxz`           | **列信息** → codec / 体积 / 原始文件名（只读）                |
| `upxz -t <FILE>.upxz`           | **测试** → 校验 magic + 往返解压（只读）                      |
| `upxz -c <orig> -o <packed>`    | **打包** → 自解压二进制到指定输出路径                          |
| `upxz --bin <inner> <a.tar.zst>`| **bin 运行** → 不解包整个 `.tar.zst`，只跑其中指定条目        |

## 安装

**预编译二进制**（Linux x86_64/arm64、macOS x86_64/arm64、Windows x86_64 —— cosign 签名，附 `SHA256SUMS`），见 [最新 release](https://github.com/ljh-sh/upxz/releases/latest)：

Linux 构建为 **musl 静态链接** —— 同一个二进制可在 Alpine **和**所有 glibc 发行版（Ubuntu/Debian/Fedora/Arch）上运行，无 `libc` 动态依赖。

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

> **`cargo install upxz`（来自 crates.io）只有打包器，没有 SFX 运行时。**
> `cargo publish` 的 tarball 会丢弃内嵌的 SFX 配套 crate（`stub/`/`loader/`/`winstub/`），
> 所以从 crates.io 装的版本无法生成自解压产物（`upxz <FILE>`）—— 会报错提示。
> `-d`/`-l`/`-t`/`--bin` 对已有的 SFX 仍可用（读嵌入式容器不需要 stub）。
> 要生成 SFX，请用 release 二进制或 `cargo install --git`（两者都带完整源码）。
> 见 [#11](https://github.com/ljh-sh/upxz/issues/11)。

## 用法

```bash
# 打包（zstd，默认 level 19）—— 生成自解压二进制（chmod +x）
upxz notes.txt                   # -> notes.txt.upxz（自解压）

# 直接运行自解压二进制（这才是真正的“运行”—— upxz 没有运行模式）
./notes.txt.upxz                 # 解压并执行原始文件；透传退出码
./notes.txt.upxz -- --flag value # -- 之后的参数原样转给原始程序

# 查看 / 校验 / 还原（多为只读）—— 全部自动定位嵌入式容器
upxz -l notes.txt.upxz           # 列信息：codec / 体积 / 原始文件名
upxz -t notes.txt.upxz           # 测试：magic + 往返解压
upxz -d notes.txt.upxz           # 解包：还原原始字节（-> notes.txt）；可执行文件会自动 chmod +x
upxz -d notes.txt.upxz -f        # 覆盖已存在的 notes.txt

# 选压缩级别
upxz --fast notes.txt            # zstd level 1 —— 最省 CPU，高频调用
upxz -z 9 notes.txt              # zstd level 9（任意 1..=19）
upxz --gz notes.txt              # 用 gzip 而非 zstd（嵌入式 magic 里 codec id 为 1）

# 打包到指定输出路径（仍是自解压二进制，只是换了个名字）
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
