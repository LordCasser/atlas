# `examples/` 测试语料仓库清单

`examples/` 目录不在版本控制内（见 `.gitignore`），存放的是公开开源项目的副本，供 Atlas 的集成测试与端到端回归使用。本文件列出所有需要的仓库及其用途，方便在 `examples/` 丢失时重建。

## 何时需要这些仓库

- **从 clone 状态跑 `cargo test --workspace`**：不需要。所有依赖 `examples/` 的测试在语料缺失时会**跳过并记录日志**（见 `example_source_or_skip!` 宏与 `corpus_or_skip!` 宏），而非编译失败或测试失败。
- **跑真实项目回归**：需要按下方清单克隆并索引。

---

## 仓库清单

| 目录 | 来源 | 需要的文件/子路径 | 用途 |
|---|---|---|---|
| `redis/` | https://github.com/redis/redis | `deps/jemalloc/src/jemalloc_cpp.cpp`、`deps/hdr_histogram/hdr_histogram.c`、`utils/redis-copy.rb`，以及全量索引 | C/C++ CFG 回归、Ruby trace、jemalloc C++ 异常帧、e2e 电池 |
| `elasticsearch/` | https://github.com/elastic/elasticsearch | 全量目录（e2e 遍历含 `.atlas/atlas.db` 的项目） | Java 大仓库基准、e2e_tests |
| `typescript_example/` | TypeScript 示例项目（4054 文件，2644 入库） | 全量目录 | TS 性能基准、resolution 正确性基准 |
| `arkts_example/` | ArkTS 示例项目（788M） | 全量目录 | ArkTS 解析回归 |
| `opencode/` | https://github.com/anomalyco/opencode | `packages/core/src/shell.ts`、`packages/sdk/js/src/v2/gen/core/queryKeySerializer.gen.ts` | TypeScript `??=` 与 `[key, value] of entries` 真实语料的 SQLite/Trace 回归 |
| `c_example/` | C 示例项目（725 文件） | `src/tool_convert.c`、`src/hdr_histogram.c` 等 | C CFG、fallthrough、label 边回归 |
| `c_sharp_example/` | https://github.com/shadowsocks-backup/shadowsocks-csharp | `shadowsocks-csharp/Controller/Service/Listener.cs`、`shadowsocks-csharp/Controller/FileManager.cs` | C# 直接 goto、CFG 回归 |
| `cangjie_example/` | Cangjie 语言示例 | `src/command_install.cj`、`src/stdx/command.cj` | Cangjie 解析回归 |
| `go_example/` | Go 示例项目 | `context.go`、`gin.go` | Go CFG 回归 |
| `java_example/` | AndroidAPKTool 子模块集合 | `brut.j.util/src/main/java/brut/util/BrutIO.java`、`brut.j.util/src/main/java/brut/util/Jar.java`、`brut.j.xml/src/main/java/brut/xml/XmlUtils.java` | Java CFG、trace 回归 |
| `python_example/` | Python 示例项目（含 `WikipediaSpider`） | 全量目录 + 需先 `atlas index` 生成 `.atlas/atlas.db` | MCP integration_tests（16 个测试）、e2e FocusRuntime 回归 |
| `rust_example/` | Rust 示例项目 | `src/less.rs`、`src/controller.rs`、`src/vscreen.rs`、`src/line_range.rs`、`tests/syntax-tests/source/PHP/test.php` | Rust CFG、let-else、PHP 语法回归 |

## 重建步骤

1. 按上表克隆各仓库到 `examples/` 对应目录名下。
2. 对需要 `.atlas/atlas.db` 的项目（至少 `python_example`，以及 e2e 遍历的 `redis`/`elasticsearch` 等），在工作目录下运行：
   ```bash
   cd examples/<repo>
   /path/to/atlas index --analysis full
   ```
3. 运行 `cargo test --workspace --all-features`，确认依赖 `examples/` 的测试不再被跳过。

## 路径标识符说明

以下字符串出现在测试代码中，但**不是磁盘上的真实文件**，而是 `FileId::generate` 使用的虚拟路径标识。实际源码由 `example_source_or_skip!` 从对应仓库读取后传入 `index_files`：

- `examples/Listener.cs` ← `c_sharp_example/shadowsocks-csharp/Controller/Service/Listener.cs`
- `examples/tool_convert.c` ← `c_example/src/tool_convert.c`
- `examples/hdr_histogram.c` ← `redis/deps/hdr_histogram/hdr_histogram.c`
- `examples/php_syntax.php` ← `rust_example/tests/syntax-tests/source/PHP/test.php`

## 不依赖 `examples/` 的测试

以下测试虽然路径含 `examples/` 字样，但实际在临时目录写入 minimal 源码，不读取真实语料：

- `crates/atlas-mcp/src/tools/integration_tests.rs::focus_equivalence_elasticsearch`（写入 minimal Java class 到 `std::env::temp_dir()`）
