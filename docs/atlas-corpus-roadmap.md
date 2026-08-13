# 从 Elixir 到 Corpus：面向 LLM Agent 的多版本源码语料库技术路线

> 本文探讨 Atlas 的一个独立未来方向——**Corpus**：受 Elixir Cross Referencer 启发、面向 LLM Agent 的多版本大型源码语料库系统。当前 Atlas 主线聚焦单项目单版本，并通过 15 个 MCP 工具提供查询能力。

---

## 1. 两个 "Elixir"，两种不同的问题

在继续之前，需要澄清一个重要的混淆点——本文讨论的 Elixir **不是** 那个基于 Erlang VM 的函数式编程语言，而是由法国嵌入式 Linux 公司 [Bootlin](https://bootlin.com) 开源的 **Elixir Cross Referencer**——一个用于索引和浏览 Linux 内核、U-Boot、BusyBox 等大型 C/C++ 项目所有历史版本的源码交叉引用系统。

你可以在 [elixir.bootlin.com](https://elixir.bootlin.com) 看到它：输入一个内核函数名，它会告诉你这个函数在哪些文件的哪一行定义、在哪些地方被调用、这个函数从哪个内核版本开始出现——而且这些查询跨**数千个版本**（Linux 内核从 2.6.11 到最新的 6.x，每一个 rc 版本和 stable 版本都索引了），响应时间在亚秒级。

这是一个惊人的工程成就：Linux 内核的完整索引数据仅约 12GB，加上 2GB 的裸 Git 仓库，总共不到 15GB 的磁盘占用就覆盖了 1800+ 个版本的完整交叉索引。

Atlas 的 roadmap 第 6.2 节明确写道：

> A multi-version source corpus system for Linux/U-Boot/BusyBox-style repositories remains a separate future product line:
>
> **Atlas: project-relative path + local workspace DB**
> **Corpus: git blob + version/tag/path mappings**

本文就是这份未来产品线的技术蓝图。我们将深入剖析 Elixir 的设计思想，提取值得继承的核心架构，说明哪些地方需要创新——特别是面向 LLM Agent 的 MCP 输出设计——并给出从 0 到 1 的详细演进路线。

---

## 2. Elixir Cross Referencer 的设计哲学

### 2.1 架构全貌

Elixir 的架构可以概括为三层：

```
┌──────────────────────────────────────────────────┐
│  Layer 3: Web / REST API                          │
│  (web.py, api.py, Falcon WSGI, Jinja2 templates)  │
│  HTML 页面渲染、autocomplete、API 端点              │
├──────────────────────────────────────────────────┤
│  Layer 2: Python 查询 & 索引逻辑                   │
│  (query.py, update.py, data.py)                   │
│  Berkeley DB 读写、identifier 查询、索引构建        │
├──────────────────────────────────────────────────┤
│  Layer 1: Shell 胶水层                            │
│  (script.sh)                                      │
│  Git 底层操作、ctags 解析、tokenization             │
└──────────────────────────────────────────────────┘
```

> **技术背景：Berkeley DB** 是 Oracle 开发的嵌入式键值数据库，诞生于 1990 年代，比起 SQLite 更早出现在 Unix 世界里。它不提供 SQL 查询，而是直接暴露 B-tree、Hash、Queue 等底层数据结构，应用层自己管理序列化和反序列化。Elixir 选择它而不是 MySQL/PostgreSQL 的原因很简单：零配置、零运维、单文件数据库、Python 原生绑定。

这种三层架构在当时（2012 年前后）是合理的工程选择，但当今的 Rust 生态已经可以提供更优解——这是 Corpus 要升级的技术基础。

### 2.2 核心洞察：用 Git Blob 去重

Elixir 最精妙的设计，也是最值得 Corpus 继承的思想，是**基于 Git blob 的索引去重**。

#### 背景知识：Git 的内容寻址存储

Git 的底层是一个内容寻址的文件系统。当你 `git add` 一个文件，Git 会：

1. 对文件内容计算 SHA-1 哈希（新版已迁移到 SHA-256）。
2. 以该哈希为键，将压缩后的内容存储在 `.git/objects/` 中。
3. 这个以内容哈希为标识的存储单元就叫 **blob**（binary large object）。

关键性质：**相同内容的文件在 Git 中永远只存一份，共享同一个 blob hash**。无论这个文件出现在多少个分支、多少个 tag、多少个 commit 中，只要内容不变，blob 就只有一个。

#### Elixir 如何利用这一性质

考虑 Linux 内核中一个稳定模块的源文件——比如 `drivers/usb/core/hub.c`。在从 v4.0 到 v6.0 的数百个版本中，这个文件可能只被修改过十几次。其他版本中的内容是**完全相同的**。

如果用传统方式："每个版本都 checkout 一份源码，对每个文件运行 ctags/解析器"——这 100 多个未改变的版本会做 100 多次完全相同的解析工作。

Elixir 的做法是：

1. **按 tag 发现版本**：`git tag` 列出所有标签，每个 tag 代表一个版本。
2. **按 blob 分配内部 ID**：遍历每个 tag 下的所有文件，对每个文件取 Git blob hash。如果这个 hash 之前没见过，分配一个新的单调递增整数 `blob_id`，记录在 `blobs.db` 中。如果见过，直接复用已有的 `blob_id`。
3. **只解析新 blob**：定义提取（ctags）、引用收集（tokenization）只对 `blob_id` 不在索引中的 blob 进行。
4. **版本文件映射另存**：`versions.db` 记录每个 tag 包含哪些 `(blob_id, path)` 对。

结果：在 Linux 全量索引中，**80% 以上的文件内容会跨版本去重，解析成本随 tag 数量增长是亚线性的**。

```text
Version v5.0:     Version v5.1:     Version v5.2:
  hub.c ──────────── hub.c' ────────── hub.c'    (v5.1 修改过，新 blob)
  sched.c ───────── sched.c ───────── sched.c'   (v5.2 修改过，新 blob)
  usb-storage.c ─── usb-storage.c ─── usb-storage.c  (从未修改！同一 blob)
```

### 2.3 查询模型：版本过滤 + 全局 postings

Elixir 的查询模型同样精妙。它不维护"每个版本的独立符号表"，而是：

1. **全局 postings**：`definitions.db` 和 `references.db` 以 identifier（符号名）为键，存储该符号在**所有 blob 中**的位置信息。每条记录是 `(blob_id, type, line_number, family)`。
2. **版本可见性**：`versions.db` 存储每个 tag 所包含的 `(blob_id, path)` 集合。
3. **查询时求交集**：查询 "v6.8 中 `schedule()` 的定义和引用" 时：
   - 从 `definitions.db` 读出所有名为 `schedule` 的 postings（全局维度）
   - 从 `versions.db` 读出 v6.8 包含的所有 blob_id 集合
   - 只保留 blob_id 在版本集合中的 postings
   - 通过 `filenames.db` 将 blob_id 映射回文件路径

**关键优势：时间复杂度由版本内文件数量决定，而非版本数量。** 无论索引了 10 个还是 1000 个版本，查询 `schedule()` 在任一版本中的信息都是 O(该符号出现次数)，而非 O(版本数)。

### 2.4 极简数据结构

Elixir 的数据结构设计也是重要参考。它使用 **packed strings** 而非规范化表：

```python
# definitions.db 中每条记录格式：
# "blobId1TypeLineFamily,blobId2TypeLineFamily#family1,family2"
```

- `DefList` 将类型码、行号、文件族信息打包进一个字符串
- `PathList` 是简单的 `"idx path\n"` 格式
- `RefList` 是 `"idx:lines:family\n"` 格式

这种设计看似简陋，实则高效——节省了关系数据库的 join 开销，将查询变成了简单的**有序列表归并**。它牺牲了真正的语义引用解析能力（函数指针、宏展开、重载解析），换取了极致的索引速度和存储效率。

---

## 3. 为什么 Atlas 需要进化出 Corpus

当前 Atlas 的定位是：

> 一个开发者在自己项目的 `.atlas/` 目录下运行 `atlas index`，得到一个本地知识图谱，然后通过 CLI/MCP 查询符号定义、调用链、数据流。

这已经很好地解决了 **"理解当前这个代码库"** 的问题。但 LLM Agent 面临的问题往往跨越版本边界：

| 场景 | Atlas 当前能力 | 需要的 Corpus 能力 |
|------|---------------|-------------------|
| CVE-2024-XXXX 影响分析 | 仅分析当前版本 | 追溯漏洞函数在哪些版本存在、何时引入、何时修复 |
| "这个 API 从哪个版本开始支持？" | 无法回答 | 函数首次出现版本、签名变化 timeline |
| 内核模块跨版本对比 | 需要手动 checkout 不同版本分别索引 | 一个命令输出两个版本间的函数 diff |
| "帮我找到所有调用 `copy_from_user` 的驱动代码" | 仅在当前项目有效 | 跨整个内核树搜索，含历史版本 |

Corpus 的目标不是替代 Atlas，而是**正交扩展**：

```text
Atlas:  使用项目相对路径 + 本地 workspace DB
        → 适合 "我当前正在开发的这个项目"

Corpus: 使用 Git blob + version/tag/path 映射
        → 适合 "我需要分析这个大型项目的全部历史"
```

两者共享解析核心（`atlas-engine`），但使用不同的索引模型和查询接口。

---

## 4. Corpus 需要的技术升级

在继承 Elixir 核心思想的同时，Corpus 需要在多处进行实质性升级：

### 4.1 解析器：从 ctags 到 tree-sitter

Elixir 依赖 Universal Ctags 作为唯一解析来源。ctags 可以快速提取函数名、行号、类型，但信息粒度有限：

- 只能获取定义行，无法获取函数体范围
- 输出的类型码是单字符（`f` = function 函数 / `s` = struct 结构体 / `v` = variable 变量 / `m` = macro 宏 / `t` = typedef 类型别名 / `e` = enumerator 枚举值 / `d` = macro parameter 宏参数），不区分方法/构造函数/宏展开，也不区分结构体和类的差异
- 没有作用域信息，同名符号在不同文件/命名空间中无法区分
- 不支持 PHP、Ruby、Kotlin、Cangjie 等现代语言

Corpus 应直接复用 Atlas 已有的 **tree-sitter 解析 + SCM 查询 + slot-based language frontend 标准化** 核心：

```text
Elixir:   ctags -x → 单字符类型码(function/struct/variable/macro/...) + 行号 + 文件名
Corpus:   tree-sitter + .scm queries →
           SymbolDef(name, kind, range, signature, exported, container)
           + DataNode(parameter, local, return, call_arg)
```

这里的“复用”不包括 Atlas workspace 身份和 DB 管线。高层
`Engine::extract_file_with_mode(SourcePath, ...)` 会按单 workspace 的项目相对路径生成
`FileId`，Corpus 禁止调用它。Corpus 只调用无 DB 的低层
`extraction::extract_file_with_mode`，由 blob OID 派生自己的稳定 ID，并将
tag/version/path 保留在独立 mapping 层。parser path 只用于语法与诊断上下文，
不能成为 blob 身份。

这带来了两个 Elixir 不具备的核心能力：

1. **函数范围索引**：记录函数的 `start_byte`、`end_byte`、`body_range`。这对于 Agent 精确提取函数源码至关重要。Elixir 只能告诉你 `schedule()` 在 `kernel/sched/core.c` 第 6503 行定义，但你不知道这个函数体到哪一行结束。
2. **函数体 hash**：对函数体内容计算 `body_hash`。这引出了下一节的核心能力。

### 4.2 函数演化分析

有了函数体 hash，Corpus 可以回答一系列 Elixir 无法回答的问题：

```text
corpus_function_first_seen("napi_schedule", project="linux")
→ "First appeared in v2.6.24 (commit 4d5e7a3)"

corpus_function_timeline("schedule", project="linux")
→ [
    {version: "v4.0", body_hash: "a1b2...", delta: "baseline"},
    {version: "v4.12", body_hash: "c3d4...", delta: "+18 lines, -5 lines"},
    {version: "v5.15", body_hash: "e5f6...", delta: "signature changed"},
    {version: "v6.1", body_hash: "g7h8...", delta: "+42 lines, refactor"}
  ]
 
corpus_diff_function("schedule", v1="v5.15", v2="v6.1", project="linux")
→ unified diff of the two function bodies
```

> **技术背景：Body Hash 归一化** —— 原始函数体 hash 对空格、注释、缩进敏感。实际实现中应提供两种 hash：
> - `raw_body_hash`：对原始字节计算 BLAKE3。用于快速判等。
> - `normalized_body_hash`：去除注释、统一空白字符后计算。用于检测 "实质相同" 的函数在不同文件中出现，或判断重构是否真正改变了语义。

### 4.3 存储层：从 Berkeley DB 到 SQLite + 压缩位图

Elixir 的 Berkeley DB 设计有历史原因，但它在多线程并发、查询灵活性和运维工具方面都不如 SQLite。Corpus 的存储方案：

```text
SQLite（结构化元数据）
  ├── corpus_blobs:        blob 元数据 (git_oid, size, family, 索引状态)
  ├── corpus_versions:     version 元数据 (tag_name, release_date, 排序键)
  ├── corpus_version_files: version × path → blob_id 映射
  ├── corpus_definitions:  全局 symbol postings (blob_id, kind, line, family)
  ├── corpus_references:   全局 reference postings
  └── corpus_functions:    函数级详细信息 (range, body_hash, signature)

Roaring Bitmap（版本文件集合加速）
  └── 每个 version → Bitmap<blob_id>
      用于快速交集计算："symbol postings ∩ visible blobs"
```

> **技术背景：Roaring Bitmap** 是现代数据分析系统中广泛使用的压缩位图数据结构。传统的 `HashSet<blob_id>` 在存储 60000+ 个 blob_id 时需要约 480KB 内存，而 Roaring Bitmap 利用内核代码的 blob_id 通常是连续的（相邻文件的 blob_id 相近）这一性质，可以将同样的集合压缩到数 KB。在 Postings × Version 交集计算中，CPU 密集的位置匹配被转化为高效的位运算。

### 4.4 查询加速：有序 postings + Segment 文件

对于 Linux 内核级别的符号（如 `printk` 有数千个引用位置），按行扫描 `definitions.db` 不再高效。Corpus 可以引入 **segment 文件**：

```text
symbol_index/
  p/
    pr/
      printf.seg       → compressed postings (blob_id + varint lines)
      printk.seg       → 同上
  s/
    sc/
      schedule.seg
      sched_clock.seg
```

每个 `.seg` 文件存储一个符号在**所有版本所有 blob** 中的出现位置，使用：

- **Varint 编码**：小整数使用变长编码，比定长 `u32` 节省 50-75% 空间。
- **Delta 编码**：相邻 blob_id 之间的差值通常很小（同一目录下的文件被分配相邻的 blob_id），存储差值而非绝对值。
- **跳表（Skip List）**：在 postings 中嵌入同步点，允许跳跃式扫描和快速交集。

> **技术背景：Posting List & Skip List** —— 这两个概念来源于全文搜索引擎（Lucene、Elasticsearch）。Posting list 是 "某个词在哪些文档中出现" 的倒排列表。Skip list 是在 posting list 中每隔 N 个元素插入一个跳转指针，使得两个 posting list 求交集时不必逐元素比对，而是 "大幅跳跃" 缩小搜索范围。

### 4.5 符号字典：FST 加速 autocomplete

Elixir 的 `/acp` autocomplete 使用 Berkeley DB cursor 的 `DB_SET_RANGE` 做前缀扫描。在 Corpus 中，如果符号字典达到 Linux 内核级别（数十万符号），可以使用 **FST (Finite State Transducer)**：

> **技术背景：FST** 是一种特殊的有限状态自动机，它以 Trie 为基础，但将 "值" 编码为状态转移的权重。这意味着一个 FST 可以同时做**前缀搜索**和**模糊匹配**。Lucene 和 Tantivy 都使用 FST 作为术语字典。以 Rust 生态的 [`fst`](https://crates.io/crates/fst) crate 为例，它可以在一棵占用仅数 MB 内存的 FST 中存储百万级别的符号，并在微秒级完成前缀搜索。

```rust
// FST 在 Rust 中的典型用法
use fst::{Set, SetBuilder};

let mut build = SetBuilder::memory();
build.insert("schedule").unwrap();
build.insert("scheduler_tick").unwrap();
build.insert("sched_setaffinity").unwrap();
let set = build.into_set();

// 前缀搜索 "sched" 的所有符号
let stream = set.starts_with("sched");
```

---

## 5. 服务人类：Elixir 兼容的 Web/API 层

Corpus 的第一类用户是**人类开发者**。Elixir 已经为此树立了标杆——在 [elixir.bootlin.com](https://elixir.bootlin.com) 上，每天有数千名内核开发者浏览源码、查找符号定义和历史引用。十年间形成的 URL 约定和 API 格式已经成为事实上的"内核源码浏览标准"，Chrome 书签、Shell 脚本、IDE 插件、CI 工具——大量下游工具直接依赖这些 URL。

Corpus 不需要重新设计一套人类界面标准。它应该**兼容 Elixir 的接口契约**，让既有工具零成本迁移，同时用 Rust 生态替代 Elixir 的 Python/Perl/Shell 实现，获得更好的性能和部署体验。

### 5.1 优先：URL 路由兼容

以下七条核心路由是 Elixir 的"功能骨架"，Corpus 必须支持：

| 路由 | 用途 | 内核开发者典型使用场景 |
|------|------|----------------------|
| `GET /` | 已索引项目列表 | 浏览 "有哪些项目可以查" |
| `GET /{project}/{version}/source` | 版本源码根目录 | 进入某个内核版本的目录树 |
| `GET /{project}/{version}/source/{path}` | 源码浏览页面 | **最核心路径**——查看带语法高亮和符号链接的源码 |
| `GET /{project}/{version}/source/{path}?raw=1` | 原始源码下载 | `curl` 或 `wget` 获取未渲染的源码 |
| `GET /{project}/{version}/ident/{ident}` | 符号交叉引用（默认 C family） | 查 `schedule` 的定义位置和所有调用点 |
| `GET /{project}/{version}/{family}/ident/{ident}` | 带 family 的符号交叉引用 | 查 Kconfig 中的 `CONFIG_HZ` 或 DTS 中的 `compatible` |
| `GET /{project}/{version}/ident` | 标识符搜索表单页面 | 手动输入符号名搜索 |

其中**源码浏览页面**最值得关注。Elixir 的源码渲染不仅仅是语法高亮——它通过一系列 **filter** 将 `#include <linux/sched.h>` 变成可点击链接、将 `CONFIG_HZ` 链接到对应的 Kconfig 定义、将 `compatible = "fsl,imx6q"` 链接到 DeviceTree binding 文档。这种 "源码超文本化" 是内核开发者日常工作中的效率倍增器。

Corpus 的渲染管线可以用 Rust 重实现：

```text
原始源码
  → tokenize/annotate（识别符号、include、config、compatible）
  → syntax highlight（syntect 或 tree-sitter-highlight）
  → linkify（将已索引的 identifier 替换为超链接）
  → line anchors（每行生成 #L123 锚点供引用）
```

### 5.2 REST API 兼容：程序的入口

对自动化工具和脚本而言，Web 页面不如 JSON API 友好。Elixir 的两条核心 API 必须兼容：

```text
GET /api/ident/{project}/{ident}?version={version}&family={family}

→ 返回该符号在指定版本中的所有定义位置、引用位置、文档注释：
{
  "definitions": [
    {"path": "kernel/sched/core.c", "line": 6503, "type": "function"}
  ],
  "references": [
    {"path": "kernel/sched/fair.c", "line": "1123,2145,3342", "type": null}
  ],
  "documentations": [
    {"path": "kernel/sched/core.c", "line": 6488, "type": null}
  ]
}

GET /acp?q={prefix}&f={family}&p={project}

→ 符号名自动补全，供搜索框使用：
["schedule", "scheduler_tick", "sched_setaffinity"]
```

需要注意的是 JSON 格式中的若干兼容细节——`line` 字段在某些场景下是整数，在多个引用共处一个文件时是逗号分隔的字符串；`type` 可以是 `null`。这些看似边缘的行为差异正是兼容性中最容易忽略但下游最依赖的部分。

### 5.3 行为约定

URL 语义中隐含的行为约定同样关键：

- **`latest` 语义**：`/linux/latest/source/...` 应重定向到 **最新已索引的 stable 版本**，不是 git 上最新的 tag，也不是 `HEAD`。这个差异很重要——上游可能发布了新 tag 但还未索引，返回未索引版本会导致查询失败。
- **`latest-rc` 语义**：同理，重定向到最新已索引的 release candidate。
- **family 回退**：当 family 参数无效时，默认回退到 `C`（C/C++/ASM）而非报错——这是 Elixir 的防御性设计，因为大多数内核查询场景就是 C 代码。
- **标识符未找到**：返回适当的 HTTP 状态码（如 404），而不是空列表，让脚本能区分 "没有这个符号" 和 "API 出错"。

---

## 6. 服务 Agent：MCP-Native 工具集

如果说 Web/API 是 Corpus 对 Elixir 的兼容承诺，那 MCP 工具集就是 Corpus 对 Elixir 的**本质超越**。一个为人类浏览器设计的 Web 页面可以展示 50 行搜索结果，但 LLM Agent 的上下文窗口需要精打细算——每个 token 都在消耗注意力。这意味着 MCP 工具的输出设计需要一套与 Web/API 完全不同的范式。

### 6.1 上下文预算感知

Agent 场景下，每个 Corpus MCP 工具都必须声明三类预算上限，并在超限时告知 Agent 而非静默丢弃：

| 预算类型 | 含义 | 示例 |
|---------|------|------|
| `max_results` | 最多返回多少条记录 | 搜索结果最多 20 个符号 |
| `max_lines` | 每项最多展示多少行 | diff 输出最多 200 行 |
| `max_bytes` | 响应总字节数上限 | 单次响应不超过 64KB |

当日志超限，工具会在响应中设置 `truncated: true` 并提示 Agent 如何缩小查询范围（例如限定文件路径、缩小版本范围、只查定义不求引用）。这种"控制透明"避免了 Agent 基于不完整数据得出错误结论。

### 6.2 双输出模式

MCP 响应结构应同时包含两个通道：

```json
{
  "structuredContent": { ... },   // 机器可解析的 JSON
  "summary": "## napi_get_frags\n\n- First seen: v4.0\n- Last changed: v6.1\n..."  // 人类可读 Markdown
}
```

`structuredContent` 供 Agent 做结构化推理（例如遍历 timeline 数组、按 body_hash 分组、计算影响范围），`summary` 在需要展示给用户或做非结构化推理时使用。两者来自同一查询结果，保证一致性。

### 6.3 精度必须声明，不得伪装

版本级别的索引有一个微妙的精度陷阱：

```text
corpus_function_first_seen("napi_get_frags", project="linux") → "v4.0"
```

这个 `v4.0` 的含义是 **"该函数的当前 body_hash 在 v4.0 tag 中首次出现"**，**不是**"该函数在 commit abcdef12 中被引入"。同一个函数可能在 v3.x 分支中被 backport、可能在一个 merge commit 中首次暴露、可能经历了一次 rebase 改变 body_hash——corpus 只能保证 tag/release 级别的首次出现。

每个涉及时间/版本的 MCP 工具输出，都应附带 `precision` 字段：

```json
{
  "first_seen": "v4.0",
  "precision": "release_tag",
  "note": "This is the first indexed tag containing this body_hash. The actual commit that introduced this function may differ."
}
```

这种诚实的不确定性比伪装精确更有价值——Agent 知道边界在哪里，就可以决定是否需要借助其他工具（如 `corpus_git_blame`）进一步定位。

### 6.4 建议工具集与设计动机

```text
# ── 版本导航（探索入口） ──
corpus_list_versions       → 有哪些版本可用？最新 stable 是哪个？
corpus_latest_version      → 快速定位 "最新" 而无需枚举
corpus_get_source          → 我想看某个版本某个文件的原始内容
corpus_get_dir             → 浏览该版本的目录结构

# ── 符号搜索（跨版本定位） ──
corpus_search_ident        → 这个符号在哪些版本的文件中出现过？
corpus_autocomplete        → "sched" 开头有哪些函数？(Agent 探索式查询)

# ── 函数级分析（Elixir 不具备的核心能力） ──
corpus_get_function        → 提取函数完整源码 + 精确的起止范围
corpus_function_timeline   → 从 4.0 到 6.1，这个函数什么时候改了？改了什么？
corpus_diff_function       → 给我看 v5.10 和 v5.15 中这个函数的 diff
corpus_function_first_seen → 我手上的 body_hash 最早出现在哪个 release？
corpus_function_blame      → 这几行是谁写的？精确到 commit

# ── 版本级变更分析（CVE 追溯的核心助手） ──
corpus_changed_functions   → 5.10 和 5.15 之间哪些函数改了？
corpus_added_functions     → 5.15 新增了哪些函数？
corpus_removed_functions   → 哪些函数在 5.15 被删除了？

# ── Linux 专用能力 ──
corpus_search_compatible   → "fsl,imx6q-uart" 这个 compatible string 在哪些 DTS 中用到了？
corpus_kconfig_value       → "CONFIG_HZ" 在哪些 Kconfig 文件中定义？默认值是多少？
```

这些工具不是随意列举的——它们对应 CVE 分析的标准工作流：

```
明确漏洞符号名 → 查 timeline 定影响版本范围 → 
diff 新旧版本看修复细节 → 查 changed_functions 识别关联变更 →
blame 定位引入 commit → 输出分析报告
```

### 6.5 实例：一次 CVE 分析的完整 MCP 交互

假设 Agent 收到一个 CVE 任务：

> "CVE-2024-XXXX：`napi_get_frags()` 在 Linux 5.15 之前存在 use-after-free 漏洞，影响范围包括 4.0 到 5.10 的所有版本。"

Corpus MCP 交互流程如下（每一步都在预构建索引中完成，无需 git checkout）：

```
── Step 1: 确定影响版本范围 ──
Agent → corpus_function_timeline("napi_get_frags", project="linux")

Corpus → {
  symbol: "napi_get_frags",
  timeline: [
    {version: "v4.0",  body_hash: "abc123", type: "first_seen"},
    {version: "v4.20", body_hash: "abc123", type: "unchanged"},
    {version: "v5.10", body_hash: "abc123", type: "unchanged"},  // 漏洞最后存在的版本
    {version: "v5.15", body_hash: "def456", type: "changed"},     // body_hash 改变 = 修复点
    {version: "v6.1",  body_hash: "ghi789", type: "changed"},
  ],
  precision: "release_tag",
  truncated: false
}

Agent 推理：body_hash 从 v5.10 到 v5.15 发生变化 → 5.15 是修复版本，4.0-5.10 受影响。

── Step 2: 查看修复内容 ──
Agent → corpus_diff_function("napi_get_frags", v1="v5.10", v2="v5.15")

Corpus → {
  diff: "@@ -127,6 +127,8 @@ static int napi_get_frags(...)\n..."
  added_lines: 12,
  removed_lines: 3,
  precision: "release_diff"
}

Agent 从 diff 中确认：v5.15 增加了一个引用计数检查，修复了 use-after-free。

── Step 3: 排查关联变更 ──
Agent → corpus_changed_functions(v1="v5.10", v2="v5.15", project="linux", filter="napi")

Corpus → {
  changed: [
    {name: "napi_get_frags", path: "net/core/dev.c"},
    {name: "napi_gro_receive", path: "net/core/dev.c"},
    {name: "__napi_schedule", path: "net/core/dev.c"}
  ],
  total: 3,
  truncated: false
}

Agent 分析：除了目标函数，napi_gro_receive 和 __napi_schedule 也在同一版本被修改，可能属于同一 patcheset。

── Step 4: 定位引入 commit（降级到 git data） ──
Agent → corpus_function_blame("napi_get_frags", version="v5.10", project="linux")

Corpus → {
  blame: [
    {line: 127, hash: "a1b2c3d4", author: "John Doe", date: "2015-03-15", summary: "net: add napi frags helper"},
    ...
  ],
  precision: "commit"
}

Agent → 输出最终报告：
  ✅ 漏洞函数：napi_get_frags
  ✅ 影响范围：v4.0 (first_seen) ~ v5.10 (last unchanged)
  ✅ 修复版本：v5.15 (body_hash changed to def456)
  ✅ 修复方式：增加引用计数检查 (+12 lines, -3 lines)
  ✅ 关联变更：napi_gro_receive, __napi_schedule
  ✅ 引入 commit：a1b2c3d4 ("net: add napi frags helper", 2015-03-15)
```

整个过程 Agent 的推理链完全由 Corpus 预构建索引支撑，四次工具调用完成一次完整的 CVE 影响分析。对比纯手工方式（下载源码、checkout 多个版本、grep、diff、blame），效率提升以数量级计。

---

## 7. 详细技术路线

### Phase 1: 基础索引管道（Rust 重写替代 Python/Perl/Shell）

**目标**：用 Rust 实现 Elixir 的索引核心，验证 blob 去重模型能在 Rust 生态中运行。

**工作项**：

1. **`corpus-core` crate**：
   - `BlobStore`：git2 绑定，`git blob hash → BlobId` 映射，blob 内容提取
   - `VersionIndex`：`git tag → [BlobId]` 映射，tag 排序/分组策略
   - `CorpusDb`：SQLite schema 实现（blobs, versions, version_files, definitions, references, functions）
   - `VersionPolicy` trait：Linux kernel、U-Boot、BusyBox 各自实现

2. **复用 Atlas 解析管线**：
   - 重用 `atlas-engine` 中无 DB 的 tree-sitter/SCM/slot frontend 抽取 API
   - 由 blob OID 生成 caller-owned ID，不调用 path-based 高层 `Engine` 抽取方法
   - 新增 `CorpusAdapter` trait，将 `FileFacts` 映射为 `blob-centric` 的 `CorpusFacts`
   - 对 C/C++ 增加宏展开后的 post-pass（`SYSCALL_DEFINE`、`EXPORT_SYMBOL` 等）

3. **索引入口 `corpus-cli`**：
   ```
   corpus init linux
   corpus remote add torvalds <url>
   corpus remote add stable <url>
   corpus sync          # git fetch --all --tags
   corpus index         # 构建/更新索引
   ```

**里程碑**：成功索引 Linux kernel 3 个 tag，查询延迟 < 100ms。

### Phase 2: 多版本数据模型与查询引擎

**目标**：实现版本可见性过滤、全局 postings 查询、函数级索引。

**工作项**：

1. **Postings 存储**：
   - `PostingsWriter`：Delta 编码 + Varint，写入 segment 文件
   - `PostingsReader`：Skip list 加速的 postings 扫描
   - Segment merge：定期合并小 segment 降低碎片

2. **Roaring Bitmap 集成**：
   - 每个 version → `RoaringBitmap<blob_id>` 序列化到 SQLite BLOB
   - `version_bitmap ∩ symbol_postings` 高效交集

3. **函数体索引**：
   - 从 tree-sitter 解析结果提取函数体 range
   - 计算 `raw_body_hash` 和 `normalized_body_hash`
   - 存储到 `corpus_functions` 表

4. **查询引擎 `corpus-query`**：
   - `QueryContext { project, version, family }` 抽象
   - `search_ident()` — 符号定义/引用查询
   - `get_function()` — 函数源码提取
   - `autocomplete()` — FST 前缀搜索

**里程碑**：成功索引 Linux kernel 100 个 tag，存储 < 5GB，查询延迟 < 200ms。

### Phase 3: Elixir 兼容 Web/API

**目标**：实现 Elixir 兼容的 URL routes 和 REST API，人类用户无迁移成本。

**工作项**：

1. **`corpus-web` crate**（基于 axum）：
   - 完全兼容 Elixir URL routes（source, ident, family/ident）
   - 源码渲染：syntect/tree-sitter-highlight 语法高亮 + linkify filters
   - Jinja2 模板到 minijinja 迁移

2. **`corpus-api` endpoints**：
   - `/api/ident/{project}/{ident}` JSON 响应（兼容 Elixir 格式）
   - `/acp` autocomplete JSON 响应

3. **latest/latest-rc 重定向**：
   - 查询已索引版本，重定向到最新 stable/rc

**里程碑**：`http://localhost:8080/linux/latest/source/kernel/sched/core.c` 可用，外观和行为与 elixir.bootlin.com 一致。

### Phase 4: MCP Agent-Native 工具

**目标**：实现面向 Agent 的 MCP 工具集，保证输出预算可控。

**工作项**：

1. **`corpus-mcp` crate**（基于 rmcp）：
   - 18+ MCP 工具，每个带 `max_results`/`max_lines`/`max_bytes` 参数
   - 双输出模式：`structuredContent`（JSON）+ `summary`（Markdown）
   - 截断标记和诊断

2. **函数演化工具链**：
   - `corpus_function_timeline`：基于 body_hash 分组，识别实质修改 vs 空白变化
   - `corpus_diff_function`：inline diff，输出适合 Agent 上下文窗口的紧凑格式
   - `corpus_function_first_seen`：发布版本级别的首次出现

3. **跨版本变更分析**：
   - `corpus_changed_functions`：两个版本间 body_hash 不同的函数集合
   - `corpus_added_functions` / `corpus_removed_functions`

**里程碑**：Agent 可通过 MCP 分析 CVE 影响范围，无需本地 git checkout。

### Phase 5: 高级分析能力

**目标**：commit-level 辅助、Linux-specific 扩展、性能优化。

**工作项**：

1. **Commit-level 辅助**（降级能力）：
   - `corpus_git_blame`：利用 git blame 建立 release first-seen → commit introduced-by 的映射
   - `corpus_git_pickaxe`：`git log -S` / `git log -G` 封装
   - 明确标注精度边界（release 级别精确，commit 级别降级到 git data）

2. **Linux 专用扩展**：
   - DTS compatible 字符串索引和查询
   - Kconfig/Makefile 专用解析
   - `corpus_search_compatible` 工具

3. **性能优化**：
   - 增量索引：只处理自上次索引以来的新 blob
   - 分段 segment merge：后台合并小 segment
   - 分布式查询：project-level 数据隔离支持多实例部署

---

## 8. 架构决策与权衡

### 8.1 为什么不用 Rust 的 Berkeley DB binding？

Berkeley DB 的 Rust 绑定生态不成熟，且 BDB 的多线程模型（需要显式管理 DBEnv、Transaction、Cursor 生命周期）与 Rust 的 ownership 模型存在摩擦。SQLite 在 Rust 中有 `rusqlite` 这一成熟的绑定库，且 Atlas 已经在 SQLite 上积累了丰富的实践经验。

### 8.2 为什么不像 SourceGraph 那样用 PostgreSQL？

SourceGraph 的系统架构依赖 PostgreSQL + Redis + 多服务协同，需要相当的运维能力。Corpus 继承 Elixir 的 "单二进制、零运维" 哲学：一个 `corpus` 二进制程序包含 Web 服务器、MCP 服务器和索引引擎。SQLite + 压缩 segment 文件是本机部署的最优解。

### 8.3 精度边界必须透明

Corpus 和 Elixir 一样，不是编译器。C/C++ 的宏展开、函数指针解析、重载决议、模板实例化——这些都不是 Corpus 的承诺范围。Corpus 定位为：

> 源码级交叉引用 + 函数级版本分析 + best-effort 语义事实

所有输出带信心值，边界能力通过诊断信息告知 Agent（而非静默失败）。

### 8.4 许可证隔离

Elixir 自身是 AGPLv3 许可证，而 Atlas 主项目是 MIT。Corpus 可以兼容 Elixir 的 URL/API 行为（行为兼容不构成版权侵犯），但**不应直接复制** Elixir 的模板、CSS、JS、图片等前端资源。如果希望提供类似的前端体验，需要独立实现或仅参考交互结构。

---

## 9. 总结：三个项目的分工关系

```text
               ┌─────────────────────────────┐
               │    atlas-engine              │
               │  (tree-sitter 解析核心)       │
               │  14 语言 typed frontend slots │
               └──────────┬──────────────────┘
                          │
          ┌───────────────┼───────────────┐
          │               │               │
          ▼               ▼               ▼
  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
  │  Atlas CLI   │ │  Atlas MCP   │ │              │
  │  (单项目单版本) │ │  (Agent 工具)  │ │              │
  └──────────────┘ └──────────────┘ │   CORPUS     │
                                    │              │
                                    │ CLI + Web    │
                                    │ + MCP + API  │
                                    │              │
                                    │ (多项目多版本) │
                                    └──────────────┘
```

| 维度 | Atlas | Corpus |
|------|-------|--------|
| 数据粒度 | 符号级 + 数据流级 | 符号级 + 函数范围级 |
| 版本模型 | 单版本（本地 workspace） | 多版本（Git tag） |
| 去重策略 | hash-based dirty detection | Git blob content-addressed |
| 查询维度 | 项目内符号关系（callers, callees, path） | 跨版本符号出现位置 + 函数演化 |
| 存储 | SQLite（Schema V3：28 张实体表 + 1 张 FTS5 索引，WAL） | SQLite + Roaring Bitmap + Segment files |
| 主要用户 | 开发中的 Agent/开发者 | 分析开源项目的 Agent/研究者 |
| Web 接口 | 无（MCP only） | Elixir 兼容 Web + REST API |

**一句话：Atlas 帮你理解"这段代码在做什么"，Corpus 帮你理解"这段代码在哪些版本中发生过什么变化"。**

两者的解析核心共享，但索引模型、存储策略、查询语义完全不同——这是架构上刻意为之的分离：让 Atlas 保持轻量和聚焦，让 Corpus 继承 Elixir 宝贵的工程经验，同时引入面向 LLM Agent 的原生能力。
