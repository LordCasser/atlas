# ArkTS CFG 支持推进 — 调查分析与实施方案

> **调查日期**：2026-07-11
> **目标**：基于 `typescript-to-arkts-migration-guide.md` 分析 ArkTS 约束对 CFG 构建、Dataflow 管道、语法事实提取 Pipeline 的影响，推进 ArkTS CFG 从 Unsupported → WithLimitations。
> **原则**：ArkTS 的约束不是"要检查的规则"，而是"可以依赖的不变量"——合法 ArkTS 代码比等价 TS 代码语义更简单，这些不变量可以而且应该被利用来提升分析精度。

> **实施状态**：Phase 1 已实施。ArkTS CFG 已从 Unsupported 推进到 WithLimitations(0.55)。G1-G4 缺口已修复。

---

## 1. 现有事实提取 Pipeline 与 CFG 被阻塞的原因

### 1.1 完整 Pipeline 概述

```
Source (.ets)
    │
    ▼ parser_source()  ─── normalize_struct_keywords (struct→class 等长归一化)
    │
    ▼ tree-sitter TS grammar ─── 产生 TS AST
    │
    ▼ normalize (arkts.rs)       ─── 消除 ArkUI 伪 method、Class→Struct 升级
    ├── symbols / references / scopes / imports
    ├── callsites (共享 ts_callsite_extractor)
    ├── lexical_bindings (共享 TS lexical.scm)
    ├── dataflow_builder (共享 TS dataflow_builder.scm)
    │
    ▼ SemanticBinder             ─── 统一绑定 source/scope
    │
    ▼ CFG Builder ◄── ❌ 在这里被阻塞
    │   └── extract.rs:423: capability.features.cfg.is_supported() → false
    │
    ▼ EffectComposer             ─── 依赖 CFG 节点 + Dataflow 边 → Effect（Alloc/Free/Call/FieldWrite）
    │
    ▼ Lifecycle / ScopeExit      ─── 依赖 EffectComposition
```

### 1.2 阻塞根因：双重门控

CFG 构建被**两层门控**阻止：

| 层 | 位置 | 逻辑 | 对 ArkTS 结果 |
|---|---|---|---|
| Mode gate | `mode.rs:104-106` | `matches!(self, LazyDataflow {..} \| Full)` | ✓ 通过（LazyDataflow/Full 模式可触发） |
| **Capability gate** | `extract.rs:423` | `frontend.capability.features.cfg.is_supported()` | **✗ 拒绝**（capability.rs:1021-1023 标记 Unsupported） |

**根因在 capability.rs**，不在 cfg_builder.rs。CFG 构建代码（`cfg_builder.rs:61-78`）早已为 ArkTS 配置了和 TypeScript 完全相同的 `CfgLanguageConfig`。阻挡 ArkTS CFG 的**唯一事实原因**是没有验证过正确性、没有 golden fixture。

### 1.3 CFG Builder 核心依赖分析

CFG Builder（`cfg_builder.rs`）只依赖：

```
CfgLanguageConfig {
    block_kinds, if_kinds, loop_kinds, return_kinds,
    throw_kinds, stmt_kinds, switch_kinds, case_kinds
}
```

这些全部是 **tree-sitter AST 节点类型名**（如 `"statement_block"`、`"if_statement"`、`"for_statement"`）。ArkTS 复用 TS grammar，所以 AST 节点类型**完全一致**。CFG Builder 不感知解析的是 TS 还是 ArkTS 文件。

**CFG 构建本身不需要任何 ArkTS 特化**——它只是一个按 AST 节点类型分派的递归遍历器。

### 1.4 当前 Dataflow 管道状态

ArkTS 已有数据流支持的层次：

| 管道层 | 状态 | confidence | 依据 |
|---|---|---|---|
| LexicalBindings | WithLimitations(0.60) | TS grammar fallback | arkts.rs:306-313 |
| LocalDataflow (intra_statement) | WithLimitations(0.60) | TS grammar fallback | capability.rs:1005-1010 |
| UseDef | WithLimitations(0.60) | scope-chain binding | capability.rs:1013-1018 |
| InterproceduralSummaries | WithLimitations(0.60) | ArgToParam/ReturnToCall verified | traced in fx27/fx28 |
| AppStorage StateFlow | 运行时边 | key matching | virtual_edges.rs:300-443 |
| **Cfg** | **Unsupported** | **"not implemented"** | **← 本次推进目标** |
| ScopeAwareBinding | Unsupported | | 不在本次范围 |

---

## 2. ArkTS 迁移指南中对 CFG/Dataflow/提取 Pipeline 有影响的约束

migration guide 约 80 条 `arkts-*` 规则。按对 Atlas Pipeline 影响分为三级。

### 2.1 直接影响 CFG 节点类型的规则

这些规则减少了 ArkTS 中可能出现的 AST 节点类型，从而简化 CFG Builder 需要处理的情况：

| 规则 ID | 约束内容 | 被排除的 TS 节点类型 | CFG 影响 |
|---|---|---|---|
| `arkts-no-for-in` | 禁止 `for..in` | `for_in_statement` | CFG 不会遇到 for_in_statement（但 `loop_kinds` 当前不包含它，无影响） |
| `arkts-no-with` | 禁止 `with` | `with_statement` | CFG 不会遇到 with_statement（stmt_kinds 未列出，无影响） |
| `arkts-no-generators` | 禁止生成器函数 | `yield_expression` | CFG 不会遇到 yield（当前不处理 yield，无影响） |
| `arkts-no-jsx` | 禁止 JSX | `jsx_*` 系列节点 | CFG 不会遇到 jsx 节点（stmt_kinds 未列出，无影响） |
| `arkts-no-new-target` | 禁止 `new.target` | `new_expression` 特殊形式 | CFG 层面无特殊处理，无影响 |

**结论：ArkTS 约束只减少节点类型，不引入新节点。现有 TS CFG 配置在合法 ArkTS 代码上完全正确。**

### 2.2 简化 Dataflow 解析的规则

这些规则排除了 TS 中需要复杂数据流分析的构造，使 ArkTS 的数据流更可预测：

| 规则 ID | 约束内容 | 对 Atlas Dataflow 的简化 |
|---|---|---|
| `arkts-no-var` | 只用 `let`/`const` | 所有变量绑定是块作用域（`lexical_declaration`），不存在 `var` 的函数作用域提升。LexicalBinding 可直接信任 tree-sitter 的 scope tree |
| `arkts-no-destruct-assignment` | 禁止解构赋值 | 赋值语句是一对一（`a = b`），不存在 `[a, b] = [1, 2]` 的一对多数据流。Dataflow Builder 不需要处理解构模式的多目标 assign |
| `arkts-no-destruct-decls` | 禁止解构声明 | 变量声明是一对一，不存在 `let {x, y} = obj`。LexicalBinding 只需处理单个 identifier |
| `arkts-no-destruct-params` | 禁止参数解构 | 函数参数是简单 identifier，不存在 `({a, b}) =>` 的隐式变量引入。UseDef/Dataflow 在函数边界只需处理简单参数 |
| `arkts-no-standalone-this` | `this` 仅在实例方法 | `this` 解析只存在于 `method_definition` 子树内。不需跨函数/静态方法上下文推断 `this` 的指向 |
| `arkts-no-props-by-index` | 字段访问只用 `.` | 所有字段访问是 `member_expression` 点操作符。AccessPath 不需要处理 `obj["key"]` 的字符串键解析 |
| `arkts-no-delete` | 禁止 `delete` | 不存在字段删除路径。Effect/Lifecycle 分析不需要建模 "字段被删除" 的状态转换 |
| `arkts-no-structural-typing` | 禁止结构类型 | 类型关系只来自 `extends`/`implements`/类型别名。不需要基于结构相似性推断类型兼容 |

**关键洞察：ArkTS 约束消除了 TS Dataflow 分析中最困难的不确定性问题（如解构的多目标赋值、`this` 的跨上下文推断、动态字段访问）。合法 ArkTS 的 Dataflow 可达到比 TS 更高的置信度。**

### 2.3 简化调用图分析的规则

| 规则 ID | 约束内容 | 对 Atlas Call Graph 的简化 |
|---|---|---|
| `arkts-no-func-expressions` | 只用箭头函数（`=>`） | 函数不是一等值。调用目标总是具名函数声明或箭头函数变量。不需要处理 `function` 表达式的匿名调用 |
| `arkts-no-nested-funcs` | 嵌套函数改为 lambda | 函数声明不在函数体内（但箭头函数赋值可以）。函数作用域扁平化 |
| `arkts-no-method-reassignment` | 方法不可重赋值 | `obj.method = fn` 禁止。方法调用目标在编译时确定 |
| `arkts-no-func-apply-call` | 禁止 `Function.apply/call` | 无动态 `this` 的调用。调用目标的 `this` 永远是声明时的类实例 |
| `arkts-no-func-bind` | 禁止 `Function.bind` | 同上 |
| `arkts-no-func-props` | 禁止函数属性赋值 | 函数对象布局不变 |

**关键洞察：ArkTS 调用图中的所有调用目标都在编译时可确定。不存在"函数作为值传递后调用"的模式。Call Graph 精度可显著高于 TS。**

### 2.4 影响模块解析 Pipeline 的规则

| 规则 ID | 约束内容 | 对 Atlas Import Resolution 的影响 |
|---|---|---|
| `arkts-no-require` | 禁止 `require`/`import =` | 模块导入只使用 ES `import` 语法。Import resolution 不需要处理 CommonJS `require()` |
| `arkts-no-export-assignment` | 禁止 `export =` | 只用标准 `export` 语法 |
| `arkts-no-ambient-decls` | 禁止 `declare module` | 所有模块是真实文件，不存在声明模块 |
| `arkts-no-module-wildcards` | 模块名无通配符 | 导入路径是具体文件路径 |
| `arkts-no-ts-deps` | `.ets` 可导入 `.ets/.ts/.js`，反向禁止 | **模块解析方向约束**——`.ts/.js` 文件不能 import `.ets`。Atlas 的模块依赖图可利用此约束减少无效边 |
| `arkts-no-misplaced-imports` | import 必须置顶 | 除动态 import 外，所有 import 在文件顶部。简化 import scanning |

### 2.5 不影响 Atlas Pipeline 的规则（仅 linter 关注）

以下规则影响代码合规性但不影响符号提取/引用/数据流/CFG/调用图：

`arkts-no-generators`（已涵盖）、`arkts-limited-throw`、`arkts-no-comma-outside-loops`、`arkts-no-spread`、`arkts-limited-stdlib`、`arkts-no-globalthis`、`arkts-no-symbol`、`arkts-no-as-const`、`arkts-as-casts`、`arkts-no-types-in-catch`、`arkts-no-type-query`、`arkts-identifiers-as-prop-names`、`arkts-unique-names`、`arkts-no-untyped-obj-literals`、`arkts-no-noninferrable-arr-literals`、`arkts-no-utility-types`、`arkts-no-conditional-types`、`arkts-no-mapped-types`、`arkts-no-intersection-types`、`arkts-no-typing-with-this`、`arkts-no-aliases-by-index`、`arkts-no-indexed-signatures`、`arkts-no-obj-literals-as-types`、`arkts-no-call-signatures`、`arkts-no-ctor-signatures-*`、`arkts-no-inferred-generic-params`、`arkts-no-private-identifiers`、`arkts-no-multiple-static-blocks`、`arkts-no-ctor-prop-decls`、`arkts-no-class-literals`、`arkts-implements-only-iface`、`arkts-no-classes-as-obj`、`arkts-no-prototype-assignment`、`arkts-no-ns-as-obj`、`arkts-no-ns-statements`、`arkts-no-ctor-signatures-iface`、`arkts-no-extend-same-prop`、`arkts-extends-only-class`、`arkts-no-enum-mixed-types`、`arkts-no-enum-merging`、`arkts-no-decl-merging`、`arkts-limited-esobj`、`arkts-no-definite-assignment`、`arkts-strict-typing-required`、`arkts-no-import-assertions`、`arkts-no-umd` 等。

这些规则不会在 tree-sitter AST 中产生新的或不同的节点类型，不影响现有 pipeline。

---

## 3. ArkUI Trailing-Block 对 CFG 的影响评估

### 3.1 语法形态

```arkts
build() {
    Row() {           // ← ArkUI trailing-block
        Column() {    // ← 嵌套 trailing-block
            Text("data")
        }
    }
}
```

TS grammar 将此解析为：

```
expression_statement
  call_expression (Row)
    arguments
  object              // ← trailing-block 的 { } 被 TS grammar 解读为 object literal
    method_definition // ← 嵌套的 Column() 被解读为 object 的 method
      property_identifier (Column)
      call_expression
        arguments
        string
```

### 3.2 CFG Builder 处理

在 `walk_stmt_list` 中：
- `expression_statement` 匹配 `stmt_kinds` → 产出一个 `Statement` CFG 节点 ← **可接受**
- CFG Builder **不递归进入** `expression_statement` 内部去发现 `call_expression` 或 `object`/`method_definition`
- 嵌套的 `Text("data")` 调用**不在 CFG 节点中体现**

### 3.3 影响评估

| 维度 | 评估 |
|---|---|
| CFG 结构（节点+边） | ✓ 正确。trailing-block 被建模为 Statement 节点 |
| Effect 提取（Alloc/Free/Call） | ⚠ EffectComposer 依赖 Dataflow，Dataflow 当前已有 ARKTS 测试通过（fx27/fx28），但 trailing-block 内部的调用（如 Text("data")）是否能被 Dataflow Builder 捕获取决于 TS `dataflow_builder.scm` query |
| 对 CFG 支持提升的阻碍 | **不阻碍**。trailing-block 的 CFG 结构与 TS 表达式语句的结构一致，CFG 构建本身可正常工作 |

**结论：ArkUI trailing-block 不影响 CFG 构建的正确性。其影响局限于 Dataflow/Effect 层对嵌套调用的捕获精度，这是已知限制（capability profile 已声明 "ArkUI trailing-block syntax may retain partial parse status"）。**

---

## 4. Struct 归一化对 CFG 的影响

### 4.1 归一化时序

```
Source:    "struct MainPage { build() { ... } }"
                    ↓ normalize_struct_keywords (pre-parse, byte-stable)
Normalized: "class  MainPage { build() { ... } }"
                    ↓ tree-sitter TS grammar parse
AST:        class_declaration → method_definition (build) → statement_block → ...
```

CFG Builder 看到的是 `class_declaration` 节点，包含 `method_definition`→`statement_block`。

### 4.2 CFG 构建结果

- `build_cfg_for_functions` 发现 `build` 是一个 `Method` 符号 → 调用 `CfgBuilder::build`
- `find_function_body` 找到 `statement_block` → `walk_block` 遍历子节点
- 子节点包括 ArkUI `expression_statement`（Row/Column 调用）→ Statement 节点

**结论：Struct 归一化不影响 CFG 构建。struct 和 class 的 method body 在 TS grammar AST 中完全相同。**

---

## 5. 其他已知 ArkTS 实现缺口及其对 CFG 推进的影响

| # | 缺口 | 是否阻碍 CFG 推进 | 说明 |
|---|---|---|---|
| G1 | source_extractor 对 `(Struct, ArkTS)` 映射缺失 | 否 | 只影响符号源码展示（`includeCode=true`），不影响 CFG |
| G2 | CHANGELOG RecoverySpec 幽灵条目 | 否 | 文档问题 |
| G3 | struct_simple.ets fixture 注释过时 | 否 | 文档问题 |
| G4 | `.sts` search query 别名缺失 | 否 | query parser 问题 |
| G5 | `public_field_definition` capture 未回传 TS/JS | 否 | ArkTS 独有增强 |

以上缺口**均不阻碍** CFG 推进，可独立修复或留待后续。

---

## 6. 推进方案

### 6.1 Phase 1：最小启用 CFG（Unsupported → WithLimitations）

**改动范围：精确到行**

#### Step 1：修改 Capability Profile

**文件**：`crates/atlas-engine/crates/types/src/capability.rs`

**当前**（L1020-1023）：
```rust
(
    FeatureField::Cfg,
    FeatureOverride::Unsupported(&["CFG builder not implemented for ArkTS"]),
),
```

**改为**：
```rust
(
    FeatureField::Cfg,
    FeatureOverride::WithLimitations(
        0.55,
        &[
            "CFG built via TS grammar fallback; ArkUI trailing-block expression_statements are modeled as Statement nodes",
            "switch/case and try/catch CFG subgraphs are deferred (shared TS limitation)",
        ],
    ),
),
```

**理由**：
- `0.55` confidence 略低于当前 `confidence_floor` 0.60，反映 CFG 对 ArkUI trailing-block 内部结构是 best-effort
- `switch/case`、`try/catch` 在 `cfg_builder.rs:17-21` 已声明为 deferred——这是 TS/JS/ArkTS 共享限制
- 表述比 `"not implemented"` 更诚实

#### Step 2：补充 ArkTS CFG Golden Fixture

**新增文件**：`crates/atlas-cli/tests/fixtures/arkts/cfg_basic.ets`

覆盖场景：
- 简单函数：`function f() { let x = 1; return x; }` → Entry → Statement → Return → Exit
- if-else：分支 → Branch/TrueBranch/FalseBranch/Join
- for 循环：Loop → body → LoopBack
- struct 方法：`struct Widget { build() { Row() { Text("hi") } } }` → Statement (expression_statement)
- 多语句顺序：Normal 链

**新增 expected**：`crates/atlas-cli/tests/fixtures/arkts/cfg_basic.expected.json`

#### Step 3：更新 documentation

- `docs/architecture.md`：能力表中 ArkTS CFG 从 ✗ 改为 limited(0.55)
- `docs/roadmap.md`：CFG 状态更新
- `capability.rs`：限制描述更新

#### Step 4：添加集成测试

在 `trace_fixtures.rs` 增加 `fx_arkts_cfg_basic` 测试：
- 验证 CFG 节点非空（`cfg_nodes.len() > 0`）
- 验证 Entry/Exit 节点存在
- 验证 if-else 产生 Branch+Join 节点
- 验证 for 循环产生 Loop+LoopBack 边

#### Step 5：验证现有测试不受影响

运行所有 `#[cfg(feature = "arkts")]` 测试确保无回归：
```bash
cargo test --features arkts
```

### 6.2 Phase 2：利用 ArkTS 约束提升 Dataflow 精度（confidence 0.60 → 0.70+）

Phase 1 完成后，Dataflow pipeline 已有 CFG 节点可消费。Phase 2 利用 §2.2-§2.3 中的不变量提升置信度：

| 推进项 | 依赖的不变量 | 预期收益 | 改动范围 |
|---|---|---|---|
| **LexicalBinding 精度提升** | no var（只用 let/const）、no destructuring | 变量绑定是一对一 identifier，无提升，可信任 tree-sitter scope tree | 当前已通过 TS grammar fallback，无需代码改动，仅需提升 `LexicalBindings` confidence |
| **Dataflow Builder 简化** | no destructuring、no bracket access、no delete | Assign 是一对一；字段访问是 member_expression；无属性删除 | 当前 `normalize_ts_dataflow_builder` 已处理这些节点。ArkTS 场景下可提升 confidence |
| **Call Graph 精度提升** | no func expressions、no method reassignment、no apply/call/bind | 所有调用目标编译时已知 | 当前 `ts_callsite_extractor` 已处理 call_expression。ArkTS 场景下可移除动态 this 的降级逻辑 |
| **CFG confidence 提升** | 合法 ArkTS 不包含 switch/case 和 try/catch 以外的未支持构造 | CFG 0.55 → 0.60（与 dataflow 对齐） | 补充测试后提升 |

### 6.3 Phase 3：补齐已知缺口（独立于 CFG，可并行）

| 缺口 | 改动 | 优先级 |
|---|---|---|
| G1 — source_extractor `(Struct, ArkTS)` 映射 | 加 `(Struct, ArkTS) => &["class_declaration"]` 或合并到 Class 分支 | P1 |
| G2 — RecoverySpec 幽灵条目 | CHANGELOG 标注已取代或删除 | P2 |
| G3 — fixture 注释过时 | 更新 `struct_simple.ets:1-5` | P2 |
| G4 — `.sts` 查询别名 | `query_parser.rs` 加 `"sts"` | P3 |
| G5 — 死代码 CFG 配置 | 如果 Phase 1 启用 CFG 则保留；否则删除 | 由 Phase 1 决定 |

---

## 7. 验证计划

### 7.1 CFG Golden Fixture 设计

`cfg_basic.ets` 覆盖以下控制流模式：

| 模式 | ArkTS 代码 | 预期 CFG 节点 |
|---|---|---|
| 空函数体 | `function empty() {}` | Entry → Exit（中间无 Statement） |
| 单语句 | `function one() { let x = 1; }` | Entry → Statement → Exit |
| return | `function ret(): number { return 42; }` | Entry → Return → Exit |
| if-else | `function branch(x: number) { if (x > 0) { return 1; } else { return -1; } }` | Entry → Branch → TrueBranch→Return, FalseBranch→Return → Join → Exit |
| for 循环 | `function loop() { for (let i = 0; i < 10; i++) { } }` | Entry → Loop → Statement → LoopBack, Normal → Exit |
| struct 方法 + UI | struct + build() + Row()/Text() | Statement 节点对应 expression_statement |
| 多语句顺序 | 3 个 `let` 声明 | 3 个 Statement 节点通过 Normal 边链接 |

### 7.2 回归测试

运行全部现有 ArkTS 测试：
- `golden_arkts_simple` / `golden_arkts_struct_simple` / `golden_arkts_struct_complex`
- `fx27` / `fx28` / `fx30` / `fx31`（trace）
- `arkts_app_storage_bridges_*`（e2e）
- 所有 `#[cfg(feature = "arkts")]` 单元测试

CFG 启用后，`trace` 和 `e2e` 测试可能因 EffectComposer 获得 CFG 节点而产生额外的 Effect 输出——这**不是回归**，而是**新增能力**。需要更新对应的 expected 结果。

### 7.3 风险缓解

| 风险 | 影响 | 缓解措施 |
|---|---|---|
| CFG 启用导致 EffectComposer 产生意外输出 | e2e/trace 测试 expected 结果不一致 | 先只改 capability（添加手动测试），不改 expected；确认新输出正确后再更新 expected |
| ArkUI trailing-block expression_statement 被标记为 Statement 后 Effect 提取不正确 | Effect 输出包含误导信息 | capability limitation 明确声明 "ArkUI trailing-block internal calls not modeled in CFG" |
| CFG 构建在 ArkUI 复杂 trailing-block 上 panic | 索引中断 | `build_cfg_for_functions` 已有 `unwrap_or_else` 降级逻辑（extract.rs:426-433），panic 被 catch 为 Warning diagnostic |

---

## 8. 需要的确认

推进 Phase 1 需要确认以下决策：

1. **CFG confidence 初始值**：建议 0.55（低于 dataflow 的 0.60），待补充足够的 ArkTS 特定验证后提升。是否同意？
2. **CFG 启用时机**：是否与 Phase 2（Dataflow 精度提升）一起进行，还是 Phase 1 单独先行？
3. **Expected 更新策略**：trace/e2e 测试的 expected 结果因 CFG 新增输出而需要更新——是手动审查后更新，还是先以 Warning 形式记录差异再统一更新？

---

## 9. 附录：关键代码位置速查

| 位置 | 内容 |
|---|---|
| `cfg_builder.rs:58-78` | `cfg_config` — ArkTS 与 TS/JS 共享的 CFG 配置（**已就绪**） |
| `cfg_builder.rs:269-333` | `CfgBuilder::build` — CFG 构建入口 |
| `cfg_builder.rs:725-728` | `walk_block` — 遍历函数体 |
| `cfg_builder.rs:737-844` | `walk_stmt_list` — 按节点类型分派 |
| `capability.rs:974-1034` | `ARKTS_PROFILE_SPEC` — 需要修改的 capability 定义 |
| `capability.rs:1020-1023` | **需要改动的行**：`FeatureField::Cfg` Unsupported → WithLimitations |
| `extract.rs:423-426` | CFG 构建调用点 + 降级逻辑 |
| `arkts.rs:354-368` | `arkts_frontend()` — 组装能力画像的工厂函数 |
| `arkts.rs:302-322` | `LexicalBindingSpec` — 词法绑定（共享 TS query） |
| `arkts.rs:324-346` | `DataflowSpec` — 数据流（共享 TS query） |
| `virtual_edges.rs:300-443` | AppStorage StateFlow 桥接实现 |
