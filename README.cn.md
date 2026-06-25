# upxz

[![CII Best Practices](https://bestpractices.coreinfrastructure.org/projects/0/badge)](https://bestpractices.coreinfrastructure.org/projects/0)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/ljh-sh/upxz/badge)](https://scorecard.dev/viewer/?uri=github.com/ljh-sh/upxz)

> 极简单二进制文件打包器：一个文件进、一个文件出，校验文件头 magic，用 zstd 打包。

> Tiny single-binary file packer. One file in, one file out, magic-checked, zstd-packed. — [English](README.md)

## 这是什么

`upxz` 把单个文件包进一个小容器，再用 [zstd](https://datatracker.ietf.org/doc/html/rfc8878) 压缩。范围故意收得很窄：

- **一个文件进，一个文件出。** 没有目录、没有通配符、没有批量模式。
- **校验文件头 magic。** 容器以 8 字节 magic（`UPXZ\x01…`）开头；对已经打包过的容器再次打包会被拒绝。
- **单二进制。** 静态链接 zstd，不需要 Python，没有额外运行时。

两个子命令：

| 命令                     | 作用                                                |
| ------------------------ | --------------------------------------------------- |
| `upxz pack <FILE>`       | 把 `FILE` 包成 `FILE.upxz`（magic + 原始文件名 + zstd 负载） |
| `upxz unpack <FILE.upxz>` | 校验 magic，把原始字节还原到磁盘                    |

## 安装

```bash
# 从源码
cargo install --git https://github.com/ljh-sh/upxz

# 或从 release 下载预编译二进制
# https://github.com/ljh-sh/upxz/releases
```

## 用法

```bash
# 用默认压缩档位打包（zstd level 3）
upxz pack notes.txt              # -> notes.txt.upxz

# 用 CPU 换更小的体积
upxz pack notes.txt --best -o notes.txt.upxz

# 最低 CPU，适合高频调用
upxz pack notes.txt --fast

# 还原原始字节
upxz unpack notes.txt.upxz       # -> notes.txt（文件名来自容器头）
```

### 压缩档位

upxz 把 zstd 暴露成三个具名预设，而不是裸的 `--level=N`：

| 档位      | 参数     | zstd level | 适用场景                  |
| --------- | -------- | ---------- | ------------------------- |
| default   | （无）   | 3          | 常见情况                  |
| fast      | `--fast` | 1          | 最省 CPU                  |
| best      | `--best` | 19         | 最小体积、最费 CPU        |

`--fast` 与 `--best` 互斥。

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
