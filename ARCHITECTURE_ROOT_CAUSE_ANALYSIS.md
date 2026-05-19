# Atlas 架构级根因分析报告

> 日期: 2026-05-19
> 分析范围: Atlas v0.1.0 全部核心源码
> 问题触发: TypeScript 大规模索引 (opencode, 1926 TS文件) 中 78% 文件因 FOREIGN KEY constraint failed 失败

---

## 一、问题陈述

在索引 1,926 个 TypeScript 文件时，Atlas 仅成功索引 431 个 (22%)，其余 1,495 个文件因 SQLite FOREIGN KEY constraint failed 错误导致整个文件的事务回滚。所有失败均发生在 `edges.source -> symbols(symbol_id)` 或 `callsites.caller -> symbols(symbol_id)` 的外键约束上。

---

## 二、数据流追踪

### 2.1 端到端数据流

```
                     +------------------------------------------------------+
                     |              index_discovered_files()                 |
                     |              (cli/commands/index.rs:91)               |
                     +--------------------------+---------------------------+
                                                | for each file
                     +--------------------------v---------------------------+
                     |              process_one_file()                       |
                     |              (cli/commands/index.rs:120)              |
                     +-----+----------------------------------+-----------+
                           |                                  |
            +--------------v--------------+    +--------------v--------------+
            |     extract_file()          |    |   insert_file_facts()      |
            | (extraction/extract.rs:22)  |    | (db/store.rs:630)         |
            |                             |    |                            |
            |  5 queries sequentially:    |    |  single tx:               |
            |  1. definitions -> symbols  |    |  files -> symbols -> scopes|
            |  2. references  -> refs     |    |  -> refs -> imports       |
            |  3. imports     -> imports  |    |  -> edges -> callsites    |
            |  4. scopes      -> scopes   |    |                            |
            |  5. dataflow    -> edges    |    |  * edges.source FK->symbols|
            |                             |    |  * callsites.caller FK->sym|
            |  + callsites derived from   |    +----------------------------+
            |    Call references          |
            +-----------------------------+
```

### 2.2 失败点精确定位

当 `insert_file_facts()` 在单个事务中执行写入时，`write_edges()` 尝试插入一条 `source = SymbolId_X` 的边，但 `SymbolId_X` 不存在于 `symbols` 表中。SQLite 的 FOREIGN KEY 约束检查导致该 INSERT 失败，进而整个事务回滚——**该文件的所有数据（包括已正确提取的 symbols、scopes、references、imports）全部丢失**。

---

## 三、架构缺陷分析

### 缺陷 #1: SymbolId 生成路径分裂 -- 「隐式契约」反模式

**严重程度: P0 -- 致命**

```
+-------------------------------------------------------------------------+
|                    SymbolId 的两条生成路径                                |
|                                                                         |
|  路径 A: definitions.scm -> normalize_definition()                      |
|  +-----------------------------------------------------+               |
|  | (variable_declarator name: (identifier) @def.var)   |               |
|  |   -> kind = SymbolKind::Variable                    |               |
|  |   -> SymbolId::generate(file_id, lang, name,        |               |
|  |       "variable", None)                              |               |
|  |   -> blake3(file_id, "typescript", "foo", "variable")|              |
|  +-----------------------------------------------------+               |
|                    X  两个 hash 永远不相等  X                            |
|  路径 B: dataflow.scm -> normalize_dataflow()                           |
|         -> find_enclosing_function_id()                                 |
|  +-----------------------------------------------------+               |
|  | arrow_function -> walk up -> variable_declarator     |               |
|  |   -> kind = SymbolKind::Function (硬编码 line 415)  |               |
|  |   -> SymbolId::generate(file_id, lang, name,        |               |
|  |       "function", None)                              |               |
|  |   -> blake3(file_id, "typescript", "foo", "function")|              |
|  +-----------------------------------------------------+               |
+-------------------------------------------------------------------------+
```

**本质**: 两个完全独立的代码路径各自独立决定 SymbolId 的组成参数，但它们之间**没有任何显式契约**保证一致性。`find_enclosing_function_id()` 是一个「逆向工程」——它试图重建 `normalize_definition()` 的决策，但:

1. **对箭头函数的 kind 判断不同**: `definitions.scm` 将 `const fn = () => {}` 捕获为 `Variable`，而 `find_enclosing_function_id()` 硬编码为 `Function`
2. **匿名箭头函数无定义**: `definitions.scm` 不捕获 `.map(() => ...)` 中的匿名箭头函数，但 `find_enclosing_function_id()` 为其生成 `name="anonymous", kind="function"` 的 SymbolId
3. **qualified_name 生成逻辑不同**: `normalize_definition` 使用 `qualified_name_from_node("", name, node, source)` 传入的是**被捕获的name节点**；`find_enclosing_function_id` 传入的是**parent节点**（即function_declaration/arrow_function），两者遍历的祖先层级不同

**为什么这是架构问题而非代码 Bug**: 即使修复了 kind 的硬编码，只要两条路径独立维护 SymbolId 的生成逻辑，未来任何查询的修改都可能再次引入不一致。这不是一个点 Bug，而是**缺乏单一事实来源 (Single Source of Truth)** 的系统性问题。

---

### 缺陷 #2: FK 约束作为运行时一致性检查 -- 「全有或全无」反模式

**严重程度: P0 -- 致命（放大器）**

```
+---------------------------------------------------------------------+
|            insert_file_facts() 的全有或全无事务                       |
|                                                                     |
|  BEGIN TRANSACTION;                                                 |
|    INSERT INTO files ...         <- OK                              |
|    INSERT INTO symbols (98条)    <- OK (这些数据是正确的!)            |
|    INSERT INTO scopes            <- OK                              |
|    INSERT INTO references        <- OK                              |
|    INSERT INTO imports           <- OK                              |
|    INSERT INTO edges             <- * FK VIOLATION -> ROLLBACK ALL  |
|    INSERT INTO callsites         <- (未执行)                        |
|  COMMIT;                                                            |
|                                                                     |
|  结果: 该文件的 98 个正确提取的 symbols 全部丢失!                     |
+---------------------------------------------------------------------+
```

**本质**: SQLite FOREIGN KEY 约束被用作运行时一致性检查器，但它的失败策略是**原子性回滚**——一个错误的 edge 导致整个文件的合法数据全部丢失。

这违反了两个架构原则:
1. **防御性设计**: 系统应优雅降级，而非全部丢弃
2. **关注点分离**: FK 约束是数据完整性保障，不应成为提取逻辑的隐式验证器

**Codegraph 的做法**: Codegraph 在写入前对每条边进行引用存在性检查，不存在则跳过该边但仍保留其余数据。这是一种更健壮的「最佳努力」策略。

---

### 缺陷 #3: 查询间无协调机制 -- 「孤岛式查询」反模式

**严重程度: P1 -- 高**

```
+--------------------------------------------------------------------+
|           extract_file() 的5个独立查询                               |
|                                                                    |
|  definitions.scm --> symbols[]     (独立执行，独立归一化)            |
|  references.scm  --> references[]  (独立执行，独立归一化)            |
|  imports.scm     --> imports[]     (独立执行，独立归一化)            |
|  scopes.scm      --> scopes[]      (独立执行，独立归一化)            |
|  dataflow.scm    --> raw_edges[]   (独立执行，依赖 find_enclosing_fn)|
|                                                                    |
|  X  5个查询之间没有任何交叉引用或协调机制  X                        |
|                                                                    |
|  dataflow 的 source SymbolId 必须与 definitions 的 SymbolId 匹配,  |
|  但 dataflow 查询根本不知道 definitions 查询产生了哪些 SymbolId!     |
+--------------------------------------------------------------------+
```

**本质**: `extract_file()` 顺序执行5个查询，每个查询独立归一化，但 dataflow 查询的 `source` 字段**隐式依赖**于 definitions 查询的结果。这是一个**未声明的跨查询依赖**，没有在接口层面或类型层面强制执行。

---

### 缺陷 #4: kind 参与 ID 哈希 -- 语义过载

**严重程度: P1 -- 高（根因催化剂）**

```rust
// ids.rs:150-167
pub fn generate(
    file_id: &FileId,
    language: &str,
    symbol_path: &str,
    kind: &str,          // <- kind 参与了 blake3 哈希
    discriminator: Option<&str>,
) -> Self {
    let mut parts: Vec<&[u8]> = vec![
        file_id.as_bytes(),
        language.as_bytes(),
        symbol_path.as_bytes(),
        kind.as_bytes(),    // <- "variable" != "function" -> 完全不同的 ID
    ];
    ...
}
```

**本质**: `SymbolId` 的设计假设「同一个符号路径 + 不同 kind = 不同符号」。这在语义上是正确的（一个叫 `foo` 的 variable 和一个叫 `foo` 的 function 确实是不同的符号），但它**放大了缺陷 #1 的影响**——当两条路径对同一个 AST 节点给出不同的 kind 时，产生的不是同一个符号的两个视角，而是两个完全不相交的 ID。

**关键矛盾**: TypeScript 中 `const fn = () => {}` 既是 Variable（声明层面）又是 Function（行为层面）。`SymbolId::generate` 的 kind 参数**强制选择一个视角**，但不同查询有理由选择不同视角。这不是 Bug，而是**设计决策与语言现实的不匹配**。

---

### 缺陷 #5: 索引循环的静默数据丢失

**严重程度: P2 -- 中**

```rust
// index.rs:101-113
match process_one_file(&abs_path, root, lang, store) {
    Ok(()) => { count += 1; ... }
    Err(e) => {
        eprintln!("  Warning: {} -- {:#}", rel_path.display(), e);
        // <- 继续下一个文件，但该文件的所有数据已丢失
    }
}
```

**本质**: 当单个文件索引失败时，系统仅打印警告并继续。但 78% 的文件失败意味着**整个索引结果的 78% 数据缺失**，而用户只看到一个 "431 files indexed" 的成功消息——没有明确的失败率报告，没有 "78% of files failed" 的醒目提示。

---

## 四、缺陷间的因果链

```
+--------------+     +--------------+     +--------------+
|  缺陷 #4     |     |  缺陷 #1     |     |  缺陷 #3     |
|  kind参与    |---->|  SymbolId    |---->|  查询间无    |
|  ID哈希      |放大  |  路径分裂    |根因  |  协调机制    |
+--------------+     +------+-------+     +--------------+
                            |
                     产生不匹配的
                     SymbolId
                            |
                            v
                     +--------------+
                     |  edges.source|
                     |  引用不存在的 |
                     |  symbols     |
                     +------+-------+
                            |
                     FK约束检查失败
                            |
                            v
                     +--------------+     +--------------+
                     |  缺陷 #2     |     |  缺陷 #5     |
                     |  全有或全无  |---->|  静默数据    |
                     |  事务回滚    |放大  |  丢失        |
                     +--------------+     +--------------+
                            |
                            v
              +-----------------------------+
              |  最终结果:                   |
              |  1926 个文件中仅 431 个     |
              |  (22%) 被成功索引           |
              |  78% 的数据静默丢失         |
              +-----------------------------+
```

---

## 五、与 Codegraph 的架构对比

| 维度 | Atlas | Codegraph | 差异根因 |
|------|-------|-----------|---------|
| **ID 生成** | blake3 哈希 (kind参与) | 自增 rowid + 属性字段 | Atlas 用哈希保证确定性，但引入了 kind 敏感性 |
| **查询协调** | 5个查询独立执行，隐式依赖 | 单遍 AST walk，所有节点类型一次性收集 | Codegraph 的单遍策略天然避免了跨查询不一致 |
| **写入策略** | 单事务，FK约束强校验 | 逐表写入，引用不存在则跳过 | Codegraph 用应用层逻辑替代DB层约束 |
| **失败处理** | 整文件回滚 | 跳过单条边，保留其余数据 | Codegraph 的最佳努力策略更健壮 |
| **语言适配** | 编译时 feature gate | 运行时 tree-sitter wasm 加载 | Atlas 的 feature gate 导致 C/C++/Java 未编译 |

---

## 六、修复方案建议

### 方案 A: 最小修复 (P0, 修复当前 Bug)

**目标**: 让 TypeScript 索引不再 FK 失败

1. **统一 `find_enclosing_function_id()` 的 kind 决策**:
   - 当箭头函数的祖先为 `variable_declarator` 时，使用 `Variable` 而非 `Function`
   - 当箭头函数无命名绑定时，不生成 source SymbolId (返回 None，跳过该 dataflow edge)

2. **在 `normalize_dataflow()` 中校验 source SymbolId**:
   ```rust
   // 在生成 RawEdge 之前，检查 source_sym 是否在 symbols 列表中
   let source_sym = find_enclosing_function_id(node, source, file_id, lang)?;
   if !known_symbol_ids.contains(&source_sym) {
       return None; // 跳过，而非生成一个 FK 违规的边
   }
   ```

3. **在 `insert_file_facts()` 中添加降级策略**:
   - edges 写入失败时，过滤掉 FK 违规的边后重试
   - 或在写入前对 edges/callsites 的 source/caller 进行存在性检查

**风险**: 最小，但不解决根本的架构问题

---

### 方案 B: 架构改进 (P1, 消除系统性风险)

**目标**: 建立单一事实来源，消除 ID 生成路径分裂

1. **引入 SymbolId Registry (Symbol Table)**:

```
extract_file() 执行流程:
+--------------------------------------------------------+
|  1. 执行 definitions 查询 -> symbols[]                  |
|  2. 构建 SymbolTable: HashMap<SymbolKey, SymbolId>     |
|     其中 SymbolKey = (qualified_name, kind)            |
|  3. 执行后续查询时，通过 SymbolTable 查找 source ID     |
|     - 不再独立生成，而是从 SymbolTable 中查找            |
|     - 找不到则跳过（返回 None）                          |
+--------------------------------------------------------+
```

2. **将 `find_enclosing_function_id()` 改为查找而非生成**:
   ```rust
   // 旧: 独立生成一个 SymbolId（可能与 definitions 不一致）
   fn find_enclosing_function_id(...) -> Option<SymbolId>

   // 新: 在已知 symbols 中查找匹配的 SymbolId
   fn find_enclosing_symbol_id(
       node: Node, source: &str, file_id: FileId,
       known_symbols: &SymbolTable,  // <- 新增参数
   ) -> Option<SymbolId>
   ```

3. **将 kind 从 SymbolId 哈希中移除 (breaking change)**:
   - 改为 `blake3(file_id, language, symbol_path, discriminator)`
   - kind 作为 symbols 表的属性字段，而非 ID 的一部分
   - 这消除了 kind 不一致导致 ID 分裂的问题
   - **权衡**: 同一作用域内不同 kind 的同名符号 (如 `const foo` vs `function foo`) 将产生 ID 冲突，需要 discriminator 区分

---

### 方案 C: 长期架构演进 (P2)

1. **单遍 AST Walk 替代多查询模式**: 像 Codegraph 一样，在单次 AST 遍历中收集所有节点类型，天然避免跨查询不一致

2. **写入层降级策略**: 将 FK 约束改为应用层校验，DB 层仅保留数据完整性约束作为最后防线

3. **运行时语言加载**: 将编译时 feature gate 改为运行时 tree-sitter grammar 动态加载，消除 "Language C not enabled" 问题

4. **LanguageAdapter 接口重构**: 将 `normalize_dataflow` 和 `normalize_reference` 的 `source_symbol` 参数改为接收 `&SymbolTable`，使依赖关系显式化

---

## 七、总结

| 缺陷 | 类型 | 严重程度 | 影响 |
|------|------|---------|------|
| #1 SymbolId 路径分裂 | 隐式契约反模式 | P0 致命 | 根因: 两路径 kind 不一致 |
| #2 全有或全无事务 | 失败策略反模式 | P0 致命 | 放大器: 一个错误边导致整文件数据丢失 |
| #3 查询间无协调 | 孤岛式查询反模式 | P1 高 | 促成者: dataflow 不知道 definitions 产生了什么 |
| #4 kind 参与 ID 哈希 | 语义过载 | P1 高 | 催化剂: kind 不一致 -> ID 完全分裂 |
| #5 静默数据丢失 | 可观测性缺失 | P2 中 | 掩盖者: 用户不知道 78% 数据已丢失 |

**核心洞察**: 这5个缺陷不是孤立的 Bug，而是一个**系统性架构问题**的5个症状。根因是 Atlas 的提取管线中，SymbolId 的生成分散在多个独立代码路径中，缺乏单一事实来源。修复任何一个单独的缺陷只能暂时缓解，只有建立显式的跨查询协调机制（方案 B）才能从根本上消除系统性风险。
