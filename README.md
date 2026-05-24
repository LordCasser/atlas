# Atlas

<p align="center">
  <strong>Local-first 语义知识图谱引擎 — 为 LLM Agent 而生。</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange" alt="Language: Rust">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="License: MIT">
  <img src="https://img.shields.io/badge/edition-2024-purple" alt="Rust Edition: 2024">
</p>

<p align="center">
  <a href="#什么是-atlas">概述</a> ·
  <a href="#快速开始">快速开始</a> ·
  <a href="#cli-命令">CLI</a> ·
  <a href="#mcp-服务">MCP</a> ·
  <a href="#支持的语言">语言</a> ·
  <a href="#架构设计">架构</a> ·
  <a href="#追踪与分析">追踪</a> ·
  <a href="#开发">开发</a>
</p>

---

## 什么是 Atlas？

Atlas 用 **tree-sitter** 解析你的源代码，提取**符号、作用域、引用、调用关系、
数据流和控制流**等事实，存入本地 **SQLite** 数据库，并通过 **CLI** 和 **MCP 服务**
两种方式向 LLM Agent 暴露丰富的查询能力（Claude、Cursor、Continue 等均已支持）。

它确定性强、完全本地运行，专为 **Agent 驱动的代码理解** 设计：
LLM Agent 可以查询 Atlas 来寻找调用者、追踪变量来源、分析影响面、构建上下文窗口
—— 一切基于真实的 AST 事实，而非 AI 猜测。

```
源代码       ──▶    Atlas 引擎   ──▶  LLM Agent（MCP 工具调用）
┌──────────┐  ┌──────────────────┐  ┌──────────────┐
│ Source   │  │ Atlas Engine     │  │ LLM Agent    │
│          │  │                  │  │              │
│ .ts .py  │  │ parse -> store   │  │ search       │
│ .java .c │  │ Resolve -> Graph │  │ callgraph    │
│ .cpp ... │  │ Trace -> Analyze │  │ trace/impact │
└──────────┘  └──────────────────┘  └──────────────┘
```

### 核心特性

| 特性 | 说明 |
|------|------|
| **确定性** | tree-sitter AST 提取，零 AI 幻觉 |
| **本地优先** | 所有数据保存在项目的 `.atlas/atlas.db` 中，不依赖任何云服务 |
| **增量索引** | 基于内容哈希的变更检测，仅重建修改过的文件 |
| **MCP 原生** | 通过 stdio 的 JSON-RPC 暴露 19 个工具，直接为 AI Agent 服务 |
| **丰富图谱** | 符号、作用域、引用、调用、导入、数据流、控制流边 |
| **变量追踪** | 变量来源追踪和调用路径查询，附带完整证据 |

---

## 快速开始

### 环境要求

- **Rust** 1.85+（edition 2024）
- **Git**（用于 `git ls-files` 文件发现；可自动回退到文件系统遍历）

### 安装

```bash
git clone https://github.com/<your-org>/atlas.git
cd atlas
cargo build --release -p atlas-cli --features "all-languages,mcp"
```

编译产物位于 `./target/release/atlas`。

### 索引你的第一个项目

```bash
# 初始化（创建 .atlas/ 目录）
atlas init --project /path/to/your/project

# 全量索引（解析所有源代码文件）
atlas index --project /path/to/your/project

# 查看索引状态
atlas status --project /path/to/your/project

# 搜索符号
atlas search "UserService" --project /path/to/your/project
```

> **提示：** 如果你已经在项目目录中，可以省略 `--project`，默认使用当前目录 `.`。

---

## CLI 命令

| 命令 | 说明 |
|------|------|
| `atlas init` | 初始化 `.atlas/` 目录，创建空 SQLite 数据库 |
| `atlas index` | 发现并解析项目中所有源文件 |
| `atlas sync` | 增量同步——仅重建发生变更的文件及相关关系 |
| `atlas search <关键词>` | 按名称搜索符号（FTS5 + LIKE + 模糊匹配级联） |
| `atlas status` | 显示文件数、符号数、边数等数据库统计信息 |
| `atlas files` | 列出所有已索引文件及其语言和状态 |
| `atlas context <符号>` | 构建 AI 上下文：调用者 + 被调用者 + 同侪，以 Markdown 输出 |
| `atlas trace point --file <文件> --line <行> --column <列>` | 解析指定代码位置的所有事实 |
| `atlas trace variable --file <文件> --line <行> --column <列>` | 追踪指定位置的变量来源 |
| `atlas trace caller-path --symbol <符号ID>` | 追踪某个函数的最远调用链 |
| `atlas doctor` | 诊断环境：SQLite、语法支持、Schema 健康状态 |
| `atlas mcp --project <路径>` | 启动 MCP 服务（JSON-RPC over stdio，需用 `mcp` feature 构建） |

所有命令均支持 `-p / --project <PATH>` 指定项目根目录（默认为 `.`）。

### 搜索语法

```bash
# 基础搜索
atlas search "calculate"

# 带过滤条件
atlas search "User" --limit 20
atlas search "kind:function lang:typescript handle*"
```

搜索采用三级联查策略：**FTS5 前缀匹配** → **LIKE 子串匹配** → **模糊前缀匹配**，
同时自动进行 camelCase/snake_case 归一化。

---

## MCP 服务

Atlas 内置 MCP 服务，通过 **JSON-RPC 2.0 over stdio** 为 AI Agent 暴露一组有界的、
文档完备的工具。

### MCP 工具列表

| 工具 | 说明 |
|------|------|
| `atlas_status` | 项目概览：文件/符号/边数量统计 |
| `atlas_files` | 列出所有已索引文件及其语言 |
| `atlas_search` | 按名称搜索符号（FTS5 + 模糊，支持 kind/lang/path 过滤） |
| `atlas_symbol` | 获取指定符号的详细信息 |
| `atlas_neighbors` | 获取某符号的入边/出边 |
| `atlas_callers` | 列出调用了某函数的所有函数 |
| `atlas_callees` | 列出某函数调用的所有函数 |
| `atlas_callgraph` | 从某符号出发的 BFS 调用图（可配置深度） |
| `atlas_path` | 两个符号在图中的最短路径 |
| `atlas_explore` | 符号详情 + 所有邻居边（含边类型） |
| `atlas_impact` | 影响面分析：哪些符号依赖了此符号？ |
| `atlas_context` | AI 上下文窗口：调用者 + 被调用者 + 同侪 |
| `atlas_trace_point` | 解析指定代码位置的所有事实 |
| `atlas_trace_variable` | 从指定代码位置追踪变量来源 |
| `atlas_trace_caller_path` | 追踪指定函数的最远调用链 |
| `atlas_language_capabilities` | 返回各语言的追踪/搜索/图谱能力元数据 |
| `usages` | 查找某符号的引用使用点 |
| `dependencies` | 查询某文件导入或 include 的文件 |
| `dependents` | 查询导入或 include 某文件的反向依赖 |

### 客户端配置

**Claude Desktop**（`claude_desktop_config.json`）：

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/path/to/target/release/atlas",
      "args": ["mcp", "--project", "/path/to/your/project"]
    }
  }
}
```

**Cursor**（`.cursor/mcp.json`）：

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/path/to/target/release/atlas",
      "args": ["mcp", "--project", "."]
    }
  }
}
```

**Continue / VS Code**（`mcp.json` 或 `~/.continue/config.json`）：

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/path/to/target/release/atlas",
      "args": ["mcp", "--project", "${workspaceFolder}"]
    }
  }
}
```

**opencode**（`opencode.json`）：

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "atlas": {
      "type": "local",
      "command": ["/path/to/target/release/atlas", "mcp", "--project", "/path/to/your/project"],
      "enabled": true
    }
  }
}
```

**Codex CLI**（`~/.codex/config.toml`）：

```toml
[mcp_servers.atlas]
command = "/path/to/target/release/atlas"
args = ["mcp", "--project", "/path/to/your/project"]
enabled = true
```

> **重要：** 启动 MCP 服务之前，必须先对项目执行一次 `atlas index`。MCP 服务从已有的
> `.atlas/atlas.db` 读取数据——它自身不会触发索引。

### MCP 请求示例

```json
{
  "method": "tools/call",
  "params": {
    "name": "atlas_trace_variable",
    "arguments": { "file_path": "src/app.ts", "line": 4, "column": 18, "max_depth": 20 }
  }
}
```

所有追踪工具返回统一的 `TraceQueryResponse<T>` 信封，包含 `ok`、`kind`、
`capability`、`partial_result`、`diagnostics` 和 `result` 字段。详细规范见
[Trace Contract](docs/trace-contract.md)。

---

## 支持的语言

### MVP 语言

| 语言 | 扩展名 | 能力等级 |
|------|--------|:---:|
| TypeScript | `.ts`, `.tsx` | DataflowBasic |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | DataflowBasic |
| Python | `.py`, `.pyi`, `.pyx` | DataflowBasic |
| Java | `.java` | DataflowBasic best-effort |
| C | `.c`, `.h` | DataflowBasic best-effort |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` | DataflowBasic best-effort |
| ArkTS | `.ets`, `.sts` | DataflowBasic best-effort via TypeScript grammar |

### Post-MVP 语言（包含在 `all-languages`）

| 语言 | 扩展名 | 能力等级 |
|------|--------|:---:|
| Go | `.go` | DataflowBasic best-effort |
| C# | `.cs` | DataflowBasic best-effort |
| Rust | `.rs` | DataflowBasic best-effort |
| PHP | `.php` | DataflowBasic best-effort |
| Ruby | `.rb` | DataflowBasic best-effort |
| Kotlin | `.kt`, `.kts` | DataflowBasic best-effort |

`DataflowBasic` 表示具备基础局部 bindings/dataflow/call argument/return facts；不表示完整跨函数变量来源追踪、CFG 或编译器级语义。具体支持功能、限制和 confidence_floor 以 `atlas doctor` / MCP `atlas_language_capabilities` 输出为准。

### 实验性语言（需显式启用）

| 语言 | 扩展名 | Feature 标志 |
|------|--------|-------------|
| Bash | `.sh`, `.bash` | `bash` |
| 仓颉 | `.cj`, `.cangjie` | `cangjie` |

### 构建变体

```bash
# 默认：TypeScript、JavaScript、Python，不包含 MCP 命令
cargo build --release -p atlas-cli

# 全部 MVP + post-MVP 语言
cargo build --release -p atlas-cli --features all-languages

# 全部语言 + MCP 服务
cargo build --release -p atlas-cli --features "all-languages,mcp"

# 开启实验性语言
cargo build --release -p atlas-cli --features "all-languages,mcp,bash,cangjie"
```

---

## 架构设计

### 处理管线

处理管线（6 个阶段：提取 → 后处理 → 解析 → 图谱 → 查询 → 接口）
```
┌────────────────────────────────────┐
│ 1. Extraction (tree-sitter + .scm) │
│ Per-file: symbols, scopes, refs,   │
│ imports, callsites, dataflow, CFG  │
└──────────────────┬─────────────────┘
                  ▼
┌────────────────────────────────────┐
│ 2. Post-Processing                 │
│ Scope tree, container assignment,  │
│ lexical + semantic binding         │
└──────────────────┬─────────────────┘
                  ▼
┌────────────────────────────────────┐
│ 3. Resolution                      │
│ Best-effort: builtin filter ->     │
│ scope-local -> container -> import │
│ -> project search fallback         │
└──────────────────┬─────────────────┘
                  ▼
┌────────────────────────────────────┐
│ 4. Graph Build                     │
│ Resolved refs + callsites ->       │
│ symbol_edges (Calls, Refs, ...)    │
└──────────────────┬─────────────────┘
                  ▼
┌────────────────────────────────────┐
│ 5. Query Layer                     │
│ GraphEngine (BFS/DFS) + Search +   │
│ Context Builder + Analysis + Trace │
└──────────────────┬─────────────────┘
                  ▼
┌────────────────────────────────────┐
│ 6. Interface                       │
│ CLI (10 cmds) + MCP (19 tools)     │
└────────────────────────────────────┘
```

### Crate 地图

项目以 Rust workspace 方式组织，共 **13 个 Cargo package**：`atlas-engine` facade、engine 内部 10 个 crate、`atlas-mcp` 和 `atlas-cli`。

```
crates/
├── atlas-engine          facade crate：re-export core APIs
│   └── crates/
│       ├── types         核心类型系统、ID、capability profile
│       ├── workspace     项目根目录、工作区路径、源文件路径抽象
│       ├── db            SQLite Schema + Store 读写 + Reader + 迁移
│       ├── extraction    tree-sitter 解析、.scm 查询、LanguageAdapter
│       ├── resolution    符号解析：引用消解 + include 图 + 路径别名
│       ├── graph         GraphBuilder、GraphSnapshot、GraphEngine（BFS/DFS）
│       ├── analysis      变量来源追踪 + 调用路径查询分析引擎
│       ├── search        FTS5 + LIKE + 模糊搜索 + camelCase 归一化
│       ├── context       AI 上下文构建器（调用者/被调用者/同侪）
│       └── filesync      增量同步：Git 感知文件发现 + 哈希变更检测
├── atlas-mcp             MCP JSON-RPC 2.0 服务（19 个工具）
└── atlas-cli             CLI 二进制（10 个命令）+ 集成测试
```

### 依赖方向（严格无环）

```
 atlas-cli ──▶ atlas-engine, atlas-mcp

 atlas-mcp ──▶ atlas-engine

 atlas-engine ──▶ types, workspace, db, extraction, resolution,
                  graph, analysis, search, context, filesync

 engine 内部 crate 保持自底向上的无环依赖：types/workspace/db → extraction/resolution/graph/analysis/search/context/filesync。
```

### 核心设计决策

| 决策 | 理由 |
|------|------|
| **确定性 ID**（blake3 哈希） | 幂等索引；不使用 UUID 或自增主键 |
| **SQLite + 内存图谱** | SQLite 作为持久化的 source of truth；内存图作为只读查询加速层 |
| **最佳努力解析** | 解析失败以警告形式呈现并携带置信度；不阻断索引流程 |
| **Feature 门控语言** | 每种语言是独立的 Cargo feature；不实现中心化大抽取器 |
| **MCP 一等入口** | MCP 服务是一等接口，不是 CLI 的附属功能 |
| **单一 Mutex\<Connection\>** | 简单并发模型，满足单机 Agent 使用场景 |

### 数据库 Schema

数据存储在 `.atlas/atlas.db`（Schema 版本 1）。主要表：

```
files          symbols        scopes         references
imports        symbol_edges   callsites      bindings
binding_uses   data_nodes     dataflow_edges cfg_nodes
cfg_edges      project_metadata               schema_versions
symbols_fts    （FTS5 全文索引）
```

Schema 迁移基础设施已存在，当前版本为 V1，`MIGRATIONS` 为空。V1 发布前会明确后续兼容策略；遇到无迁移路径的旧库时，按 `atlas doctor` 指引重建 `.atlas/atlas.db`。

---

## 追踪与分析

Atlas 提供**变量来源追踪**和**调用路径查询**能力——它不是完整的污点分析或漏洞扫描器。
你提供一个可疑的代码位置，Atlas 返回结构化的程序证据。

### 追踪管线

用户指定代码位置，Atlas 返回结构化证据——三种入口统一输出 `TraceQueryResponse<T>` 信封。
```
  User Query: "Where does this value come from?"
       │
       ▼
  TraceEngine
       │
       ├─ trace_point(file, line, col)
       │    Resolve facts: reference, symbol,
       │    data_node, binding, callsite, scope
       │
       ├─ trace_variable(file, line, col)
       │    Backward slice via dataflow edges
       │    from query point -> farthest origin
       │
       └─ trace_caller_path(symbol_id)
            BFS from target -> farthest caller
       │
       ▼
  TraceQueryResponse<T>
     { ok, kind, capability, partial_result, diagnostics, result }
```

### 能力门控

每种语言通过能力画像（capability profile）明确规定可用的追踪功能：

| 功能 | TS/JS | Python | Java | C/C++ | ArkTS | Post-MVP |
|------|:-----:|:------:|:----:|:-----:|:-----:|:--------:|
| 符号与引用 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 调用图 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 局部数据流 | ✓ | ✓† | ✓† | ✓† | ✓† | ✓† |
| Use-Def 链 | ✓†† | ✓†† | ✓†† | ✓†† | ✓†† | ✓†† |
| 控制流图 (CFG) | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ |

> ✓† = AST-driven local dataflow，仍有语言特定缺口 · ✓†† = 名称/作用域启发式精度 · Post-MVP = Go/C#/Rust/PHP/Ruby/Kotlin

当追踪查询超出当前语言的能力边界时，Atlas 返回 `partial_result: true` 并附带详细
诊断信息——绝不会静默地返回空结果。

详见 [Trace Contract](docs/trace-contract.md)，包含完整的 JSON 契约和使用示例。

---

## 开发

### 构建与测试

```bash
# 默认测试套件（TypeScript、JavaScript、Python）
cargo test

# 完整测试套件（全部语言 + MCP）
cargo test -p atlas-cli --features "all-languages,mcp"

# 仅集成测试
cargo test --test integration

# 构建 release 二进制
cargo build --release -p atlas-cli --features all-languages
```

### 项目结构

```
atlas/
├── crates/                # atlas-engine facade、engine 内部 crates、CLI、MCP
├── docs/                  # 架构文档、需求规格、追踪合约
│   ├── 01-requirements.md
│   ├── 02-architecture-constraints.md
│   ├── 03-current-architecture.md
│   ├── 04-changes.md
│   ├── 05-roadmap.md
│   └── trace-contract.md
├── examples/              # 各语言的测试项目
├── Cargo.toml             # Workspace 根配置
└── README.md
```

### 架构原则

- **职责分离**：每个 crate 有单一、明确的职责
- **确定性 ID**：所有 ID 均为 blake3 哈希——保证索引幂等
- **最佳努力语义**：解析错误以警告形式呈现，不阻断管线
- **Feature 门控**：语言和 MCP 通过 Cargo features 控制；sync/filesync 作为 engine 默认能力提供
- **Deref 强制转换**：`Store` 自动解引用为 `StoreReader`，清晰分离读写
- **输出预算**：所有响应受尺寸限制，不存在无界输出

---

## 已知限制

| 领域 | 限制说明 |
|------|---------|
| **追踪** | 数据流/追踪仅支持 TS/JS/Python；其他语言仅符号级 |
| **C/C++** | 不做预处理展开；仅解析 `#include` 指令 |
| **Java** | 不做 classpath/Maven/Gradle 解析；跨文件解析基于名称匹配 |
| **Python** | 不做动态类型推断；运行时构造的符号无法捕获 |
| **ArkTS** | 委托给 TypeScript 语法解析；部分 ArkTS 特有语法可能无法解析 |
| **Post-MVP 语言** | Go/C#/Rust/PHP/Ruby/Kotlin：基础 DataflowBasic best-effort；完整 path-level 追踪仍需按语言验证 |
| **Barrel 重导出** | TypeScript 重导出链通过名称兜底解析，而非 AST 导出图 |
| **性能** | 10 万+ 符号项目需全量内存图（约 50MB 内存） |
| **并发** | 单一 `Mutex<Connection>`；MCP 服务为单线程 |
| **Schema** | 已有 V1 迁移基础设施；当前 `MIGRATIONS` 为空，无迁移路径时需按 `atlas doctor` 指引重建 |

---

## 路线图

当前优先事项（详见 [`docs/05-roadmap.md`](docs/05-roadmap.md)）：

1. **稳定追踪事实**，确保变量来源追踪和调用路径查询可靠
2. **加固 TS/JS/Python 追踪 fixtures**，增强路径步骤的语义断言
3. **保持能力边界的显式化**，在 CLI 和 MCP 输出中清晰呈现
4. **轻量级函数摘要** 和有界的跨函数变量来源追踪
5. **稳定 `atlas-engine` facade API**，直至追踪精度达到生产可用水平

---

## 参与贡献

1. 提交前请运行完整测试套件：
   ```bash
   cargo test -p atlas-cli --features "all-languages,mcp"
   ```
2. 新的提取逻辑必须包含集成测试
3. 语言适配器遵循 `crates/atlas-extraction/src/languages/` 中的 `LanguageAdapter` trait
4. Schema 变更需同步更新版本追踪和测试（当前阶段无需部署迁移）
5. 修改追踪合约时，需要同步更新 `docs/trace-contract.md` 和对应测试

---

## 许可证

[MIT](LICENSE)
