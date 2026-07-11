# ArkTS 分析边界审计

> 审计日期：2026-07-11
> 范围：ArkTS parser fallback、声明式 ArkUI、CFG、dataflow、AppStorage 运行时边
> 结论：保留有限 CFG 支持；不基于语言规范约束上调分析置信度；当前不继续扩张 ArkUI parser。

## 1. 证据边界

本审计同时使用：

- 华为官方 [TypeScript 到 ArkTS 迁移规则](https://developer.huawei.com/consumer/cn/doc/harmonyos-guides/typescript-to-arkts-migration-guide)。
- 华为官方 [AppStorage 状态模型](https://developer.huawei.com/consumer/cn/doc/harmonyos-guides/arkts-appstorage)。
- 仓库 `examples/arkts_example` 的 392 个 `.ets` 文件。
- extraction、golden、trace 和 workspace 测试。

规范描述合法 ArkTS 的语言约束，但 Atlas 当前不运行 ArkTS 编译器，也不验证文件是否已经完成
TS -> ArkTS 迁移。因此，`no var`、`no destructuring`、`no function expression` 等规则不能作为
输入已经满足的可信不变量，只能用于解释语言边界。Golden fixture 证明某个模式被覆盖，不构成
全局精度校准。

## 2. 当前解析架构

```text
original .ets source
  -> ArkTS ParserSpec::parser_source
     -> byte-stable `struct` -> `class ` normalization
  -> tree-sitter TypeScript grammar
  -> ArkTS normalizers
     -> class_declaration / fallback class -> Struct
     -> brace-balanced complete Struct range
     -> reject ArkUI fake methods
     -> recover UI calls and build() ownership
  -> lexical/dataflow/CFG consumers
```

所有 parser consumer 必须复用 `ParserSpec::parser_source()`，并要求 normalization 后字节长度与
原源码完全一致。AST range 最终仍用于切原源码，不能使用改变 offset 的 source rewrite。

`SourceExtractor` 也遵守这一约束。它不能绕过 frontend 后直接以 TypeScript grammar 重解析原始
ArkTS，否则 `struct` 不会形成可识别 class。若 fallback class AST 的终点早于已存 Struct range，
AST definition 必须被拒绝并退回完整 stored range。

## 3. 已支持能力

### 3.1 声明式结构事实

- `struct`、字段、真实方法和 scope 可恢复。
- 参数化 component decorator + trailing chain 形成的 fallback `class` 也可恢复；Struct range
  使用跳过 string/template/regex/comment 的 brace balance 覆盖真实 closing brace。
- `build()` 是真实 Method，不再是全局假调用。
- `Row`、`Column`、`Web` 等 UI 调用保留 `build()` caller。
- 成员调用保留 receiver，允许识别 `AppStorage.set/setOrCreate`。
- ArkUI trailing-block 仍可能令文件状态为 `partial`；状态不伪装为 `success`。

### 3.2 CFG

ArkTS 与 TypeScript 共享 CFG node-kind 配置。以下模式已由 ArkTS golden/trace fixture 验证：

- named function/method 的顺序语句。
- `if/else` 的 Branch、Join、TrueBranch、FalseBranch。
- `for/while` 的 Loop、Statement、LoopBack。
- return 和 Entry/Exit。

能力边界为 `WithLimitations(0.55)`，不是完整 ArkUI CFG。

### 3.3 Dataflow 与 AppStorage

- TS-compatible lexical binding、local dataflow、use-def：`WithLimitations(0.60)`。
- 已解析调用的 ArgToParam / ReturnToCall：`WithLimitations(0.60)`。
- 查询时 `StateFlow`：
  `AppStorage.set/setOrCreate(key, value)` -> 同 key 的
  `@StorageProp/@StorageLink` 字段读取和外层 UI call argument。
- 字段必须是精确的 `this.<decorated field>`；`other.<same name>` 不桥接。
- key 匹配保留 literal / expression 类别，字符串 `'StorageKey.X'` 不等于表达式
  `StorageKey.X`。表达式未解析到常量值，因此 confidence 不高于 `0.60`。
- 冷 Focus 通过 `StateChannel` closure 查找跨目录 writer；resume replay 时由 Engine 为匹配 key 的
  writer function materialize dataflow，终态路径不依赖预建 full index。

`StateFlow` 不写入函数内 `dataflow_edges`，也不创建虚构 AppStorage 实体；它由
`RuntimeEdgeProvider` 在查询时生成。

## 4. 明确未支持的边界

### 4.1 ArkUI trailing-block 内部控制流

TypeScript grammar 会把 `Column() { ... }` 的尾随 block 解释为 ERROR/object/method 组合。
normalizer 能恢复调用事实，但 CFG walker 只看到外层 Statement，不能证明 block 内完整控制流。

### 4.2 嵌套 arrow callback

真实 ArkUI 大量使用：

```arkts
.onBackPressed(() => {
  if (condition) { ... }
  return true
})
```

当前 arrow function 有 lexical scope，但匿名 callback 没有独立 Symbol，因此不会获得独立
function CFG。把 callback 的分支直接递归合并进 `build()` CFG 会混淆执行时机和函数边界，禁止
采用这种伪修复。

### 4.3 其他共享限制

- switch fall-through、try/catch/finally、async/await 和标签跳转仍是 best-effort/deferred。
- Atlas 不验证 ArkTS 编译合法性，不能因官方禁止某语法而假设输入中不存在该语法。
- interface/polymorphic dispatch、framework callback 和函数值传递仍可能产生未解析调用。
- AppStorage 不建模 `@StorageLink` 反向写回、字段默认初始化、常量求值、时序和进程边界。

## 5. 为什么当前不继续扩张 parser

当前原始目标是让声明式 UI 的组件、`build()`、UI Sink 和 AppStorage 入向来源可查询；这条主链已有
端到端测试。剩余缺口集中在“ArkUI block/callback 的完整语义”，正确推进至少需要以下之一：

1. 可维护的 ArkTS/ArkUI tree-sitter grammar；或
2. callback 独立 symbol + call edge + function CFG 的通用模型。

两者都属于跨语言架构能力，不应通过更多 ArkTS 字符串 rewrite、伪 method 或把 callback CFG
并入 owner method 来实现。在出现依赖 UI callback path-sensitive 分析的明确产品需求前，不增加
grammar fork 或匿名 callback 实体。

## 6. 继续推进的触发条件

满足任一条件后重新评估：

- 真实查询要求区分 ArkUI 条件渲染的不同路径。
- Sink 位于 callback 内，现有 call/dataflow facts 无法建立可解释路径。
- 需要 callback 注册关系或 callback 内 lifecycle/branch analysis。
- 可获得稳定、版本可控且覆盖 ArkUI declarative syntax 的 grammar。

推进时必须先用真实 ArkTS corpus 建立 parse/status、symbol ownership、callback 和 CFG 基线，再决定
是 grammar 路径还是通用 callback IR；不得先提高 capability 再补实现。

## 7. 验证清单

- `cargo test -p atlas-engine source_extractor`
- `cargo test -p atlas-cli --test golden golden_arkts`
- `cargo test -p atlas-cli --test trace_fixtures arkts`
- `cargo test -p atlas-cli --test trace_e2e arkts_app_storage`
- `cargo test --workspace`
