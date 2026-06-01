# Domain Rules 通用化 — 架构设计修正（v2）

> 状态: 提交评审  
> 日期: 2026-06-02  
> 评阅人: software-architect  
> 修正范围: `docs/task-lazy-experience.md` §4a Domain Rule System  
> 影响模块: `analysis/src/domain_rules.rs`, `db/src/store/domain_rules.rs`, `db/src/schema.rs`  
> v2 变更: 采纳用户评审意见，核心完全语言无关化，引入 Registry + status + pattern_kind + meta_version

---

## 1. 问题诊断

### 1.1 当前设计的问题

`docs/task-lazy-experience.md` §4a 中描述的 Domain Rule System 存在两个层次的问题：

**问题 A：概念混淆** — 文档和代码将 "domain rules" 等同于 "C/C++ 所有权规则"。 `LoadedDomainRules` 的字段名（free_functions / allocation_functions / owned_field_patterns / cleanup_functions）和 `schema.rs` 的表注释（"ownership rules for lifecycle analysis"）都在暗示 domain_rules 是 C/C++ lifecycle 的子系统——但它不应该是。

**问题 B：抽象缺失** — 核心 rule engine 缺乏语言无关的抽象层：
- 没有 `pattern_kind` 列，engine 无法区分 exact / prefix / glob 匹配策略
- 没有 `status` 列，learned rule 的生命周期（candidate → enabled → disabled → rejected）无法表达
- 没有 `meta_version`，不同语言 plugin 的 meta 演进无法管理
- 没有注册校验机制，rule_kind 自由文本缺少拼写错误防护

### 1.2 Domain Rule 的本质

```
domain_rules 不是 ownership rules。
domain_rules 不是 C/C++ lifecycle 的子系统。

它是语言无关的规则存储、匹配、学习候选、审计基础设施。
所有语义都由 language plugin 和 consumer 解释。
C/C++ ownership 只是注册到这个系统里的第一组 rule kinds 和第一个 consumer。
```

这一认知修正会改变整个模块的命名、分层和依赖方向。

### 1.3 各语言 Domain Rule 的实际用途

| 语言 | 需要的 rule_kind | 示例 pattern |
|------|-----------------|-------------|
| **C/C++** | free_fn, alloc_fn, owned_pattern, cleanup_fn | `Curl_safefree`, `aprintf` |
| **Rust** | unsafe_boundary, drop_impl, trait_contract | `transmute`, `ManuallyDrop::drop` |
| **Python** | context_manager, resource_factory, close_method | `open`, `requests.Session` |
| **Go** | defer_cleanup, error_wrap, ctx_pass | `defer resp.Body.Close()` |
| **TypeScript** | react_hook, state_setter, effect_cleanup | `useEffect`, `useState` |
| **通用** | deprecated, error_propagate, side_effect | `@deprecated`, `log.Fatal()` |

---

## 2. 修正后的架构

### 2.1 核心原则

1. **核心完全语言无关**：`domain_rules` crate 的核心只认 `Rule`, `RuleSet`, `RuleKind`, `Language`, `Pattern`, `RuleMatch`, `RuleSource`, `RuleStatus`, `RuleMetadata`。不出现 `free`, `alloc`, `ownership`, `cleanup`, `lifecycle`。
2. **语言语义通过 Registry 注入**：每个语言实现 `LanguageRuleKinds` trait 注册自己的 rule_kind 和 builtin 规则。核心 engine 只管 match + validate。
3. **Consumer 解释 RuleMatch**：C/C++ lifecycle consumer 识别 `rule_kind = "free_fn"` 的匹配结果并解释为释放语义。核心不知道也不关心。
4. **Learning 插件化**：`RuleLearningStrategy` 是 language plugin 的一部分。核心只负责 persist candidate + mark `source=learned, status=candidate`。
5. **不做跨语言规则继承**：`language = "*"` 的通配仅用于极少数通用模式，不做复杂继承链。

### 2.2 Schema 设计（长期版本）

```sql
CREATE TABLE domain_rules (
  id            TEXT PRIMARY KEY NOT NULL,
  language      TEXT NOT NULL,           -- "c" / "rust" / "python" / "typescript" / "*"
  rule_kind     TEXT NOT NULL,           -- 语言自由定义, 由 Registry 校验
  pattern       TEXT NOT NULL,           -- 匹配目标 (函数名 / 字段路径 / 装饰器名 ...)
  pattern_kind  TEXT NOT NULL DEFAULT 'exact',  -- exact / prefix / suffix / glob / regex
  meta          TEXT,                    -- JSON, 语言特定扩展
  meta_version  INTEGER NOT NULL DEFAULT 1,     -- meta 结构版本, 支持演进
  source        TEXT NOT NULL,           -- builtin / learned / user
  status        TEXT NOT NULL DEFAULT 'enabled', -- candidate / enabled / disabled / rejected / deprecated
  confidence    REAL NOT NULL DEFAULT 1.0,
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

-- pattern_kind 的语义：
--   exact     — 精确字符串匹配 (如 "Curl_safefree")
--   prefix    — 前缀匹配 (如 "ng_*" → ngAfterViewInit, ngOnDestroy)
--   suffix    — 后缀匹配 (如 "*_handler" → error_handler, signal_handler)
--   glob      — glob 模式 (如 "set[A-Z]*" → setState, setValue)
--   regex     — 正则表达式 (高级场景)

-- status 的生命周期：
--   candidate  → enabled   (用户 approve learned rule)
--   candidate  → rejected  (用户拒绝 learned rule)
--   enabled    → disabled  (用户临时禁用)
--   enabled    → deprecated (规则已过时)
```

**与当前 schema 的差异**：

| 列 | 当前 | 修正后 | 说明 |
|----|------|--------|------|
| `language` | 无 | **新增** | 语言作用域 |
| `pattern_kind` | 无 | **新增** | 匹配策略, 核心 engine 职责 |
| `meta` | 无 | **新增** | 语言特定扩展 |
| `meta_version` | 无 | **新增** | meta 结构演进管理 |
| `status` | 无 | **新增** | 替代简单布尔, 支持完整生命周期 |
| `updated_at` | 无 | **新增** | 规则修改时间追踪 |

### 2.3 核心抽象（完全语言无关）

```rust
// crates/atlas-engine/crates/domain_rules/src/types.rs

/// 一条通用领域规则 — 核心不包含任何语言特定语义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainRule {
    pub id: String,
    pub language: String,         // "c" / "rust" / "*"
    pub rule_kind: String,        // 自由文本, 由 Registry 校验
    pub pattern: String,          // 匹配目标
    pub pattern_kind: PatternKind,
    pub meta: Option<serde_json::Value>,
    pub meta_version: u32,
    pub source: RuleSource,
    pub status: RuleStatus,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatternKind {
    Exact,
    Prefix,
    Suffix,
    Glob,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSource {
    Builtin,
    Learned,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    Candidate,   // learned, 待用户审批
    Enabled,     // 活跃
    Disabled,    // 用户主动关闭
    Rejected,    // 用户拒绝 (不再提醒)
    Deprecated,  // 标记为过时
}

/// 匹配结果 — 核心不解释语义, consumer 负责解释
#[derive(Debug, Clone)]
pub enum RuleMatch {
    Known {
        rule_id: String,
        kind: String,              // "free_fn" / "react_hook" / ... — 由 consumer 解释
        confidence: f64,
        meta: Option<serde_json::Value>,
    },
    Heuristic {
        rule_id: String,
        kind: String,
        confidence: f64,
        meta: Option<serde_json::Value>,
    },
}
```

### 2.4 Language Registry（语义注入层）

```rust
// crates/atlas-engine/crates/domain_rules/src/registry.rs

/// 每种语言注册自己的 rule_kind 和 builtin 规则
pub trait LanguageRuleKinds: Debug + Send + Sync {
    /// 语言标识符
    fn language(&self) -> &'static str;

    /// 此语言支持的所有 rule_kind 规格
    fn known_rule_kinds(&self) -> &'static [RuleKindSpec];

    /// 此语言的 builtin 规则 (source=builtin, status=enabled)
    fn builtin_rules(&self) -> Vec<DomainRule>;

    /// 校验一条规则的 schema 合法性
    /// 核心 engine 在 insert/update 前调用此方法
    fn validate_rule(&self, rule: &DomainRule) -> RuleValidationResult;
}

/// 一个 rule_kind 的规格定义
#[derive(Debug, Clone)]
pub struct RuleKindSpec {
    /// 唯一标识符, 如 "free_fn", "react_hook"
    pub name: &'static str,
    /// 人类可读描述
    pub description: &'static str,
    /// 此 rule_kind 是否支持自动学习
    pub auto_learn_enabled: bool,
    /// 允许的 pattern_kind 列表 (如 free_fn 只允许 exact)
    pub allowed_pattern_kinds: &'static [PatternKind],
    /// 按 source 判断默认 status
    pub default_status: fn(RuleSource) -> RuleStatus,
    /// meta 结构的简单校验规则 (不是完整的 JsonSchema, 仅关键字段检查)
    pub meta_validator: Option<fn(&serde_json::Value) -> Result<(), String>>,
}

/// 规则校验结果
#[derive(Debug, Clone)]
pub enum RuleValidationResult {
    Valid,
    Warning(String),     // 可接受但有隐患
    Rejected(String),    // 无法接受
}
```

**C/C++ Registry 示例**：

```rust
// crates/atlas-engine/crates/domain_rules/src/kinds/c.rs
// feature-gated: #[cfg(any(feature = "c", feature = "cpp"))]

use super::*;

pub struct CRegistry;

impl LanguageRuleKinds for CRegistry {
    fn language(&self) -> &'static str { "c" }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "free_fn",
                description: "释放内存或资源的函数",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[PatternKind::Exact],
                default_status: |src| match src {
                    RuleSource::Builtin => RuleStatus::Enabled,
                    RuleSource::User    => RuleStatus::Enabled,
                    RuleSource::Learned => RuleStatus::Candidate,
                },
                meta_validator: None, // free_fn 的 meta 暂时无结构约束
            },
            RuleKindSpec {
                name: "alloc_fn",
                description: "分配内存或资源的函数",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[PatternKind::Exact],
                default_status: |src| match src {
                    RuleSource::Builtin => RuleStatus::Enabled,
                    RuleSource::User    => RuleStatus::Enabled,
                    RuleSource::Learned => RuleStatus::Candidate,
                },
                meta_validator: None,
            },
            RuleKindSpec {
                name: "owned_pattern",
                description: "被拥有的字段模式 (如 data->state.aptr.*)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Prefix, PatternKind::Glob],
                default_status: |src| match src {
                    RuleSource::User => RuleStatus::Enabled,
                    _ => RuleStatus::Candidate,
                },
                meta_validator: None,
            },
            RuleKindSpec {
                name: "cleanup_fn",
                description: "批量清理资源的函数 (如 Curl_freeset)",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[PatternKind::Exact],
                default_status: |src| match src {
                    RuleSource::Builtin => RuleStatus::Enabled,
                    RuleSource::User    => RuleStatus::Enabled,
                    RuleSource::Learned => RuleStatus::Candidate,
                },
                meta_validator: None,
            },
        ]
    }

    fn builtin_rules(&self) -> Vec<DomainRule> {
        make_c_builtins()
    }

    fn validate_rule(&self, rule: &DomainRule) -> RuleValidationResult {
        // 1. 检查 rule_kind 是否已注册
        let spec = match self.known_rule_kinds().iter().find(|s| s.name == rule.rule_kind) {
            Some(s) => s,
            None => return RuleValidationResult::Rejected(
                format!("unknown rule_kind '{}' for language 'c'", rule.rule_kind)
            ),
        };
        // 2. 检查 pattern_kind 是否允许
        if !spec.allowed_pattern_kinds.contains(&rule.pattern_kind) {
            return RuleValidationResult::Rejected(
                format!("pattern_kind {:?} not allowed for rule_kind '{}'", rule.pattern_kind, rule.rule_kind)
            );
        }
        RuleValidationResult::Valid
    }
}
```

### 2.5 Generic Rule Engine（核心匹配层）

```rust
// crates/atlas-engine/crates/domain_rules/src/engine.rs

/// 语言无关的规则引擎
///
/// 职责：加载 enabled 规则 → 校验 → 匹配 pattern → 返回 RuleMatch
/// 不负责：解释 RuleMatch 的语义（那是 consumer 的事）
pub struct GenericRuleEngine {
    registry: HashMap<String, Box<dyn LanguageRuleKinds>>,
}

impl GenericRuleEngine {
    /// 注册一个语言插件
    pub fn register(&mut self, plugin: Box<dyn LanguageRuleKinds>) {
        self.registry.insert(plugin.language().to_string(), plugin);
    }

    /// 对指定语言的指定 rule_kind 匹配 target
    ///
    /// 查询顺序：language 精确匹配 → language="*" 通配
    /// 只返回 status=Enabled 的规则
    pub fn match_pattern(
        &self,
        store: &Store,
        language: &str,
        rule_kind: &str,
        target: &str,
    ) -> Vec<RuleMatch>;

    /// 批量匹配：一次查询多个 rule_kind
    pub fn match_patterns(
        &self,
        store: &Store,
        language: &str,
        kinds: &[&str],
        target: &str,
    ) -> HashMap<String, Vec<RuleMatch>>;

    /// 按语言 + status 列出规则 (用于 atlas domain_rules list)
    pub fn list_rules(
        &self,
        store: &Store,
        language: Option<&str>,
        status: Option<RuleStatus>,
    ) -> Vec<DomainRule>;

    /// 校验并写入规则
    pub fn upsert_rule(&self, store: &Store, rule: &DomainRule) -> Result<String, RuleValidationResult>;
}
```

**匹配逻辑**（完全语言无关，不关心 free/alloc 语义）：

```rust
fn try_match(rule: &DomainRule, target: &str) -> bool {
    match rule.pattern_kind {
        PatternKind::Exact   => rule.pattern == target,
        PatternKind::Prefix  => target.starts_with(&rule.pattern),
        PatternKind::Suffix  => target.ends_with(&rule.pattern),
        PatternKind::Glob    => glob_match(&rule.pattern, target),
        PatternKind::Regex   => regex_match(&rule.pattern, target),
    }
}
```

### 2.6 C/C++ Consumer（语义解释层）

```rust
// crates/atlas-engine/crates/analysis/src/ownership_rules.rs (新建)
// 替代原来的 domain_rules.rs 中的 C/C++ 专属部分

use domain_rules::{GenericRuleEngine, RuleMatch, RuleSource};

/// C/C++ 所有权规则视图 — 从通用引擎加载并解释为所有权语义
pub struct CppOwnershipRules {
    pub free_functions: Vec<(String, RuleSource)>,
    pub allocation_functions: Vec<(String, RuleSource)>,
    pub owned_field_patterns: Vec<String>,
    pub cleanup_functions: Vec<(String, RuleSource)>,
}

impl CppOwnershipRules {
    /// 从 GenericRuleEngine 加载 C 语言的所有权规则
    pub fn load(engine: &GenericRuleEngine, store: &Store) -> Self {
        let free     = engine.match_pattern(store, "c", "free_fn", ...);
        let alloc    = engine.match_pattern(store, "c", "alloc_fn", ...);
        let owned    = engine.match_pattern(store, "c", "owned_pattern", ...);
        let cleanup  = engine.match_pattern(store, "c", "cleanup_fn", ...);
        // 将 RuleMatch 解释为所有权语义, 构建 CppOwnershipRules
        // ...
    }

    /// 按语言加载 (向前兼容, Phase 4 v2 使用)
    pub fn load_for(engine: &GenericRuleEngine, store: &Store, lang: &str) -> Self { ... }

    /// 匹配释放函数 — C/C++ consumer 把 "free_fn" RuleMatch 解释为释放语义
    pub fn match_free(&self, func_name: &str) -> Option<RuleMatch> { ... }

    /// 匹配分配函数
    pub fn match_alloc(&self, func_name: &str) -> Option<RuleMatch> { ... }

    /// 匹配被拥有的字段模式
    pub fn matches_owned_pattern(&self, field_path: &str) -> bool { ... }
}

// 向后兼容别名 — 原有代码可以继续使用 LoadedDomainRules
#[deprecated(note = "use CppOwnershipRules instead")]
pub type LoadedDomainRules = CppOwnershipRules;
```

### 2.7 Crate 结构与依赖方向

```
domain_rules crate (新增, 语言无关核心)
├── src/types.rs        — DomainRule, RuleMatch, PatternKind, RuleSource, RuleStatus
├── src/registry.rs     — LanguageRuleKinds trait, RuleKindSpec, RuleValidationResult
├── src/engine.rs       — GenericRuleEngine (match + validate + CRUD)
├── src/store.rs        — GenericRuleStore (SQLite 持久化)
├── src/learning.rs     — RuleLearningStrategy trait (插件化学习)
├── src/pattern.rs      — PatternKind 匹配实现 (exact/prefix/suffix/glob/regex)
├── src/kinds/
│   ├── mod.rs          — Registry 聚合
│   ├── c.rs            — C/C++ Registry (feature-gated: #[cfg(any(feature = "c", feature = "cpp"))])
│   └── (future: rust.rs, python.rs, typescript.rs, ...)
└── src/lib.rs

依赖链:
  types ──► db ──► domain_rules ──► analysis
                                      │
                                      ├── ownership_rules.rs (CppOwnershipRules — C/C++ consumer)
                                      ├── lifecycle.rs       (消费 CppOwnershipRules, 不变)
                                      ├── lifecycle_proof.rs (消费 CppOwnershipRules, 不变)
                                      └── rule_learning.rs   (实现 RuleLearningStrategy for C, 移入 domain_rules)
```

`domain_rules` 不依赖 `extraction`, `resolution`, `graph`, `lazy`, `analysis`。

### 2.8 Learning 插件化

```rust
// crates/atlas-engine/crates/domain_rules/src/learning.rs

/// 语言特定的规则学习策略 — 由 language plugin 实现
pub trait RuleLearningStrategy: Debug + Send + Sync {
    /// 该策略适用的语言
    fn language(&self) -> &'static str;

    /// 从项目中扫描并发现候选规则
    fn discover_candidates(&self, store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>>;

    /// 解释一条候选规则的推理依据
    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String;

    /// 最小独立使用点数
    fn min_usage_count(&self) -> usize { 5 }

    /// 最低置信度阈值
    fn confidence_threshold(&self) -> f64 { 0.8 }
}

/// 一条学习到的候选规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedRuleCandidate {
    pub language: String,
    pub rule_kind: String,
    pub pattern: String,
    pub pattern_kind: PatternKind,
    pub usage_count: usize,
    pub confidence: f64,
    /// 学习证据 (存于 meta 的 "learned_from" 键)
    /// 格式: [{"file": "...", "symbol": "...", "line": N, "evidence_kind": "..."}]
    pub evidence: Vec<LearningEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningEvidence {
    pub file_id: String,
    pub symbol_id: Option<String>,
    pub line: u32,
    pub evidence_kind: String,  // "free_after_use", "alloc_assigned_to_field", ...
    pub confidence: f64,
}
```

**Generic layer 的职责**（只做存储，不做语义判断）：

```rust
impl GenericRuleEngine {
    /// 运行 learning strategy, 将候选规则写入 domain_rules 表
    /// 写入规则: source=learned, status=candidate, enabled=false
    /// evidence 存入 meta.learned_from
    pub fn run_learning(
        &self,
        store: &Store,
        language: &str,
    ) -> anyhow::Result<Vec<DomainRule>> {
        let strategy = self.get_learning_strategy(language)?;
        let candidates = strategy.discover_candidates(store)?;

        let mut persisted = Vec::new();
        for c in candidates {
            let rule = DomainRule {
                language: c.language,
                rule_kind: c.rule_kind,
                pattern: c.pattern,
                pattern_kind: c.pattern_kind,
                source: RuleSource::Learned,
                status: RuleStatus::Candidate,
                confidence: c.confidence,
                meta: Some(serde_json::json!({
                    "learned_from": c.evidence,
                })),
                ..Default::default()
            };
            persisted.push(rule.clone());
            self.upsert_rule(store, &rule)?;
        }
        Ok(persisted)
    }
}
```

### 2.9 未来：Evidence 独立表（Phase 4b+ 可选增强）

```sql
-- 等 learned rules 有实际用户量后，从 meta JSON 升格为独立表
-- 当前 Phase 4 不建此表，evidence 存在 meta 的 learned_from 数组中
CREATE TABLE domain_rule_evidence (
  rule_id       TEXT NOT NULL REFERENCES domain_rules(id),
  evidence_kind TEXT NOT NULL,   -- "free_after_use" / "alloc_assigned_to_field" / ...
  file_id       TEXT,
  symbol_id     TEXT,
  line          INTEGER,
  confidence    REAL NOT NULL,
  PRIMARY KEY (rule_id, evidence_kind, file_id, symbol_id, line)
);
```

---

## 3. 对 Phase 4 的修正

### 3.1 修正后的 Phase 4 范围

| 子阶段 | 内容 | 原文档 | 修正后 |
|--------|------|--------|--------|
| **4a** | Schema | C/C++ 专用 ownership 表 | 通用 `domain_rules` 表 (含 language/pattern_kind/status/meta/meta_version) |
| **4a** | Core Engine | 无（硬编码在 analysis） | `GenericRuleEngine` + `LanguageRuleKinds` trait |
| **4a** | C Registry | 无 | `CRegistry` 注册 free_fn/alloc_fn/owned_pattern/cleanup_fn |
| **4a** | C Consumer | `LoadedDomainRules` | **重命名**为 `CppOwnershipRules`，`LoadedDomainRules` 为 deprecated alias |
| **4b** | Auto-learning | 写死在 analysis crate | 实现 `RuleLearningStrategy` for C；evidence 存 meta JSON |
| **4c** | User Annotation | `atlas annotate` | 增加 `language` + `pattern_kind` 参数 |
| **4d** | Lifecycle Proof | 消费 LoadedDomainRules | 消费 `CppOwnershipRules`（API 不变，只是类型名变了） |

### 3.2 未来语言接入路径

```
Phase 4 (C/C++)
  └── CRegistry 注册 4 个 rule_kind → CppOwnershipRules 消费

Phase 4+ (Rust)
  └── RustRegistry 注册 unsafe_boundary/drop_impl/trait_contract → RustSafetyRules 消费

Phase 4+ (Python)
  └── PyRegistry 注册 context_manager/resource_factory/close_method → PyResourceRules 消费

Phase 4+ (TypeScript)
  └── TSRegistry 注册 react_hook/state_setter/effect_cleanup → ReactHooksRules 消费
```

**核心 engine 不会因为这些新 rule_kind 改一行代码。**

---

## 4. 设计理由

### 4.1 为什么这次修正进一步抽象

用户的 8 条反馈在本质上是同一个原则的展开：**从核心中剥离所有语言语义**。

v1 修正已经做到了 engine 层语言无关，但在以下几个方面不够彻底：
- `LoadedDomainRules` 命名暗示全局性
- Schema 缺少 `pattern_kind`，模糊了引擎职责和语义的边界
- 没有 registry 的 `validate_rule()`，rule_kind 自由文本无防护
- 没有 status 状态机，learned rule 生命周期表达力不足

v2 修正补齐了这些缺口，代价很低（加几列，写一个 trait），长期收益很高（加任何语言都不碰核心）。

### 4.2 为什么不做更复杂方案

**备选：Evidence 独立表现在就建**
- 否决：auto-learning 当前用户量为零，建表后无数据填充，徒增维护成本。先用 meta JSON 存 `learned_from`，有真实使用量后再升格。设计上已预留 `domain_rule_evidence` 表结构。

**备选：meta_schema 使用完整 JsonSchema 库**
- 否决：引入 `jsonschema` crate 增加了编译时间和依赖复杂度。当前验证用简单的函数指针 `fn(&Value) -> Result<(), String>` 足够覆盖关键字段检查。

**备选：rule_kind 在数据库中建 CHECK constraint**
- 否决：每加一个 rule_kind 需要 ALTER TABLE + migration。应用层 `RuleKindSpec` + `validate_rule()` 校验提供同等防护而不增加 schema 变更成本。

### 4.3 为什么不用更简单方案

**备选：保持 v1，不引入 registry/status/pattern_kind**
- 否决：v1 的核心 engine 已经是语言无关的，但 schema 和命名还没有。如果不修正命名（`LoadedDomainRules` → `CppOwnershipRules`），未来接入 Rust 时会看到 `LoadedDomainRules::load_for("rust")` 返回一个名为 `free_functions` 的字段包含 Rust 的 `transmute`——这比提前改名的代价大得多。另外 `pattern_kind` 也是核心 engine 的职责而非语言语义（匹配策略是 engine 层的行为），应该在 schema 中显式建模。

---

## 5. Risk Assessment

| 风险 | 可能性 | 影响 | 缓解 |
|------|--------|------|------|
| `LoadedDomainRules` → `CppOwnershipRules` 重命名导致编译错误 | 确定 | 低 | 保留 `pub type LoadedDomainRules = CppOwnershipRules;` deprecated alias |
| migration 新增 6 列后已有测试失败 | 低 | 低 | 新列全部有 DEFAULT 值；`language` 默认 `'c'`，`status` 默认 `'enabled'` |
| `pattern_kind` 匹配性能问题 (glob/regex) | 低 | 低 | 绝大多数规则是 `exact` (HashMap 查找)；regex 匹配有明确的上限和缓存 |
| registry 注册过多 rule_kind 导致 engine 初始化变慢 | 低 | 低 | `RuleKindSpec` 是 compile-time 常量数组，初始化 O(1) per language |
| learned rules 的 evidence 在 meta JSON 中过大 | 中 | 低 | evidence 默认最多保留 10 条，超出截断；等有真实需求时升格到独立表 |

---

## 6. Verification Criteria

### 6.1 核心语言无关性验证

- `GenericRuleEngine::match_pattern()` 的签名和实现中不出现 `free`, `alloc`, `ownership`, `cleanup`, `lifecycle` 字符串。
- `domain_rules` crate 的 `Cargo.toml` 不依赖 `extraction`, `resolution`, `graph`, `lazy`, `analysis`。
- 删除 `CRegistry` 后，`GenericRuleEngine` 的所有测试仍通过。

### 6.2 Schema 验证

- `domain_rules` 表包含 `language`, `pattern_kind`, `meta`, `meta_version`, `status`, `updated_at` 列。
- 已有数据 migration 后 `language = 'c'`, `pattern_kind = 'exact'`, `status = 'enabled'`。
- `status = 'candidate'` 或 `'disabled'` 或 `'rejected'` 的规则不会被 `match_pattern` 返回。
- `INSERT` 时 `rule_kind = "unknown_kind"` 且未注册的 language 会触发 `RuleValidationResult::Rejected`。

### 6.3 C/C++ 向后兼容验证

- `CppOwnershipRules::load()` 从 engine 加载规则后行为与修正前的 `LoadedDomainRules::from_rows()` 完全一致。
- `match_free("Curl_safefree")` 在加载了 `user` source 规则后返回 `Known`。
- `match_free("free")` 在无 user 规则时返回 `Heuristic { confidence: 0.9 }`（builtin 兜底）。
- 现有 `lifecycle.rs` / `lifecycle_proof.rs` 测试全部通过（仅需修改 `use` 路径）。

### 6.4 通用性验证

- `GenericRuleEngine::match_pattern(store, "rust", "unsafe_boundary", "transmute")` 在无注册规则时返回空 vec。
- 注册 `RustRegistry` 后插入 `rust` + `unsafe_boundary` + `"transmute"` 规则，匹配返回 `Known`。
- `language = "*"` 的通配规则在所有 language 的查询中都能返回。

### 6.5 Auto-Learning 验证

- `CRegistry` 的 `RuleLearningStrategy` 实现行为与原有 `analysis/rule_learning.rs` 一致。
- learned candidates 写入 `meta.learned_from` 包含 file_id + symbol_id + line + evidence_kind。
- learned rules 的 `status = 'candidate'`，`match_pattern` 不返回 candidate 状态规则。
- `atlas domain_rules approve <id>` 将 status 改为 `enabled` 后，`match_pattern` 开始返回。

---

## 7. Handoff to Coder

### 实施前必读

- `crates/atlas-engine/crates/db/src/schema.rs` L390-398 — 当前 `domain_rules` 表定义
- `crates/atlas-engine/crates/db/src/store/domain_rules.rs` — 当前 CRUD（需要扩展列 + status 查询）
- `crates/atlas-engine/crates/analysis/src/domain_rules.rs` — 需要拆分为 `ownership_rules.rs` (CppOwnershipRules) + 删除 C 专属逻辑
- `crates/atlas-engine/crates/analysis/src/lifecycle.rs` — C/C++ consumer（确认调用方式不变）
- `crates/atlas-engine/crates/analysis/src/lifecycle_proof.rs` — 同上
- `crates/atlas-engine/crates/analysis/src/rule_learning.rs` — 需要移入 `domain_rules::kinds::c`

### 实施顺序

1. **Schema migration**: `domain_rules` 加 `language`, `pattern_kind`, `meta`, `meta_version`, `status`, `updated_at` 六列。已有数据默认值填充。
2. **`db::DomainRuleRow`** 加对应字段 + `list_domain_rules` 增加 `language`/`status` 过滤参数。
3. **新建 `domain_rules` crate** —— 核心不包含 C/C++ 语义：
   - `types.rs` — `DomainRule`, `RuleMatch`, `PatternKind`, `RuleSource`, `RuleStatus`
   - `registry.rs` — `LanguageRuleKinds` trait, `RuleKindSpec`, `RuleValidationResult`
   - `engine.rs` — `GenericRuleEngine` (match + validate + CRUD)
   - `store.rs` — `GenericRuleStore` (SQLite 持久化)
   - `learning.rs` — `RuleLearningStrategy` trait, `LearnedRuleCandidate`
   - `pattern.rs` — pattern 匹配实现 (exact/prefix/suffix/glob/regex)
   - `kinds/c.rs` — `CRegistry` + C builtins + C RuleLearningStrategy 实现
4. **`analysis/src/ownership_rules.rs`** (新建): `CppOwnershipRules` 从 engine 加载并解释为所有权语义。`pub type LoadedDomainRules = CppOwnershipRules;`
5. **清理 `analysis/src/domain_rules.rs`**: 删除 C/C++ 专属逻辑（builtin 默认值、match_free/match_alloc 兜底），改为委托 `CppOwnershipRules`。
6. **`analysis/src/rule_learning.rs`**: 委托给 `domain_rules::kinds::c` 的 learning strategy。
7. **`analysis/src/lib.rs`**: 重导出 `CppOwnershipRules`。
8. **`atlas-engine/Cargo.toml`**: 新增 `domain_rules` 依赖。
9. **测试**: 现有 lifecycle 测试 → 全部通过；新增多语言 query + registry 校验测试。

### 禁止越界

- `domain_rules` crate 不依赖 `extraction`, `resolution`, `graph`, `lazy`, `analysis`。
- `GenericRuleEngine` 不包含任何 `free`, `alloc`, `ownership`, `cleanup`, `lifecycle` 逻辑或字面量。
- 不修改 `lifecycle.rs` / `lifecycle_proof.rs` 的公共接口签名（只改 use 路径）。
- 不对 `rule_kind` 做数据库 CHECK constraint。
- Phase 4 不建 `domain_rule_evidence` 独立表（evidence 存 meta JSON）。
