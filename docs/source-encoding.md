# 源文件编码与统一读取

本文是 Atlas **源码读盘** 的权威约定：需求、API、hash 语义、约束与必测项。  
实现：`workspace::source_text`（`read_source` / `decode_source`）。

## 1. 需求

| 项 | 约定 |
|----|------|
| 非 UTF-8 源码 | 支持识别并在**内存**中转为 UTF-8；**优先覆盖中文 GBK（及 GB18030 族）**；西欧 8-bit 走 **windows-1252**（Encoding Standard 超集，覆盖实际 ISO-8859-1 源码场景；`encoding_rs` 不单独导出 ISO-8859-1） |
| 原文件 | **只读**，永不因编码转换写回磁盘 |
| 读取入口 | 全项目**唯一**源码读入口：`workspace::read_source` / `workspace::decode_source` |
| 文件 hash | `blake3(原始磁盘字节)` → `SourceText.file_hash`；写入 `files.content_hash`、dirty、fingerprint、stale 比较 |
| 部分内容 hash | `blake3(解码后 UTF-8 的对应字节)` → `workspace::text_content_hash` / `SourceText::text_hash` |

依赖：`chardetng`（检测）+ `encoding_rs`（解码）+ `blake3`。

## 2. 解码流程

```text
fs::read(path)  ──raw──►  file_hash = blake3(raw)
                     │
                     ├─ valid UTF-8? ──yes──► text = as_str, encoding="UTF-8"
                     │
                     └─ no ──► chardetng.guess → encoding_rs.decode
                               ──► text (UTF-8), encoding=name, had_errors
```

- 合法 UTF-8（含纯 ASCII）走热路径，不跑检测。
- 检测参数：`Utf8Detection::Allow`，`Iso2022JpDetection::Deny`，`tld=None`。
- 解析层（tree-sitter、`extract_file_with_mode`）只接收解码后的 `&str`；`content_hash` 参数传入 **file_hash（raw）**。

## 3. Hash 语义（禁止混用）

| 用途 | 输入 | API |
|------|------|-----|
| 文件身份、dirty、fingerprint、`files.content_hash`、stale | **raw 磁盘字节** | `file_hash` / `file_content_hash` |
| snippet / 符号体 / 任意「内容」digest | **解码后 UTF-8** | `text_content_hash` / `text_hash()` |

对纯 UTF-8 文件：`file_hash == text_hash()`（全文）。  
对 GBK 等：二者**必须不同**；若把 `text_hash` 误写入 `content_hash`，dirty 会永远 mismatch。

## 4. TextRange 与坐标

- DB 中 `TextRange` / byte offset **相对解码后的 UTF-8 `text`**。
- 再次取源码（includeCode、dossier、trace snippet）必须走同一入口再解码，再按 range 切片。
- **禁止**用 DB range 对 raw 磁盘文件 `seek`/切片。
- 行号在换行语义一致时通常可用；列/字节不可直接映射回原 GBK 文件（不提供写回编辑器映射）。

## 5. 读文件约束（完成后强制）

### 5.1 必须使用统一入口

以下路径必须经 `workspace::read_source`（或已持有 raw 时的 `decode_source`）：

- Index / sync 单文件抽取（`filesync::index_phases`）
- Focus structural / resolution_symbols / self-heal（`focus/materialize/structural`）
- Focus dataflow materialize（`focus_materialize::loader`）
- Bootstrap hints / Tier2 manifest（`focus/bootstrap`）
- Source 摘录（`source_extractor`、`context`、`dossier::source_repo`、`analysis::trace`）

### 5.2 允许不经解码入口的情况

| 场景 | 做法 |
|------|------|
| dirty / fingerprint **仅**算文件 hash | `fs::read` + `workspace::file_content_hash`（或与 `file_hash` 等价） |
| `.atlasignore`、path_alias 配置、非源码文本 | 可继续 `read_to_string`（配置默认 UTF-8） |
| 测试读本仓库 UTF-8 fixture / golden expected | 测试代码自便；**非 UTF-8 fixture 必须用 `read_source`** |
| MCP handler 静态扫自家 Rust 源 | 开发期自检，不走产品源码管线 |

### 5.3 禁止

- 对**项目源文件**使用 `std::fs::read_to_string` 作为产品路径。
- 解码后写回磁盘「修复编码」。
- 用解码后全文 hash 充当 `files.content_hash`。
- 绕过入口各自实现 charset 猜测。

### 5.4 审查清单

合并前建议：

```bash
# 产品路径不应再出现对源码的 read_to_string（下列除外：discovery ignore、
# path_alias、测试、handler_purity）
rg 'read_to_string' crates/atlas-engine crates/atlas-mcp --glob '*.rs'
```

新增读源码调用点：只允许 `workspace::read_source` / `decode_source`。

## 6. 必要测试

分层对齐 `docs/testing.md`：§2.1 单元 + §2.3 集成（decode → extract → index/dirty）。

### 6.1 单元（`workspace`，强制）

实现：`crates/atlas-engine/crates/workspace/src/source_text.rs`（`mod tests`）。

| 测试名 | 断言 |
|--------|------|
| `utf8_chinese_preserves_text_and_dual_hash_equality` | 中文保留；`encoding=UTF-8`；`file_hash == text_hash` |
| `utf8_pure_ascii_hot_path` / `utf8_empty_file` | ASCII / 空文件热路径 |
| `gbk_chinese_decodes_to_expected_utf8` | 非 UTF-8 raw → 正文与 golden UTF-8 一致；GBK/GB18030 族 |
| `gbk_file_hash_is_raw_not_decoded` | `file_hash == blake3(raw)`；`!= text_hash` |
| `western_8bit_decodes_latin_characters` | windows-1252/ISO-8859-1 系；café/résumé/naïve |
| `text_content_hash_uses_decoded_utf8_slice` | 片段 hash 基于 UTF-8；≠ raw file hash |
| `read_source_does_not_rewrite_disk_*` | 读后磁盘字节不变 |
| `read_source_missing_file_errors` | 缺失路径返回错误 |

```bash
cargo test -p workspace --lib source_text
```

### 6.2 集成：解码 + 解析（`extraction`）

实现：`crates/atlas-engine/crates/extraction/tests/source_encoding_extract.rs`。

| 测试名 | 断言 |
|--------|------|
| `gbk_python_extract_preserves_chinese_symbol_names` | GBK 盘上 `.py` → `read_source` → Structural extract；符号名含 `计算总和`/`数据服务`/`查询`；`FileFacts.file.content_hash == file_hash`；磁盘未改 |
| `gbk_python_manifest_top_level_chinese_names` | Manifest 路径中文顶层名正确、无 U+FFFD |
| `utf8_python_still_extracts_chinese_names` | 对照：UTF-8 同逻辑源码可抽中文名；`file_hash == text_hash` |

```bash
cargo test -p extraction --test source_encoding_extract
```

### 6.3 集成：index + dirty（`filesync`）

实现：`crates/atlas-engine/crates/filesync/tests/source_encoding_index.rs`。

| 测试名 | 断言 |
|--------|------|
| `gbk_index_stores_raw_file_hash_and_chinese_symbols` | `run_index_pipeline(Structural)` 后 DB `content_hash == blake3(raw)`；符号含中文名；磁盘未改 |
| `gbk_unchanged_file_is_not_permanently_dirty` | 未改文件时 raw 重算 hash == DB；二次 index 不把 hash 改成 text_hash |

```bash
cargo test -p filesync --test source_encoding_index
```

### 6.4 一键回归

```bash
cargo test -p workspace --lib source_text \
  && cargo test -p extraction --test source_encoding_extract \
  && cargo test -p filesync --test source_encoding_index
```

改编码策略或读盘入口时必须跑通上述命令。

## 7. 实现映射

| 符号 | 说明 |
|------|------|
| `workspace::read_source` | 读盘 + 解码 + file_hash |
| `workspace::decode_source` | 已有 raw 时解码 |
| `workspace::file_content_hash` | raw → hex |
| `workspace::text_content_hash` | UTF-8 字节 → hex |
| `SourceText::{text,file_hash,encoding,had_errors}` | 返回结构 |

调用方典型模式：

```rust
let src = workspace::read_source(&path)?;
let content_hash = src.file_hash; // → DB / extract
let source = src.text;            // → tree-sitter / 摘录
```

## 8. 已知限制

- 极短/少字符的非 UTF-8 文件可能误检编码 → 标识符 mojibake（best-effort）。
- 单文件混合编码不支持。
- 西欧检测/解码使用 **windows-1252**（非独立 ISO-8859-1 常量）；与 legacy Latin-1 源码实践兼容。
- GBK 族可能报告 `GB18030`（超集，可接受）。
- 极短、汉字很少的 GBK 文件可能被误判为 Latin 单字节编码——测试与真实中文源码应含足够中文上下文。
- 不把 encoding 持久化进 schema（一期）；需要时可看 tracing / 未来 diagnostic。
