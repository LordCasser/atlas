# Domain Rules Language Guide

本文说明不同语言如何使用和扩展 Atlas 的 domain-rules 系统。架构原则见 [`architecture.md`](./architecture.md)；本文聚焦 language registry、rule_kind 命名、consumer 解释和验证要求。

## 1. Core Boundary

`domain_rules` 核心是语言无关基础设施：

- 存储规则：`domain_rules` 表。
- 校验规则：`LanguageRuleKinds` registry。
- 匹配规则：`GenericRuleEngine` + `PatternKind`。
- 学习候选：`RuleLearningStrategy` 写入 `status='candidate'`。

核心不解释规则语义。所有语义必须在 analysis consumer 中解释，例如 C/C++ 的 `CppOwnershipRules`、未来 Rust 的 safety consumer、Python 的 resource consumer 或 TypeScript 的 framework consumer。

## 2. Extension Workflow

新增语言或新增语言语义时按此顺序做：

1. 在 `domain_rules::kinds` 中新增或扩展 registry。
2. 为该语言声明 `RuleKindSpec`：`name`、描述、是否可学习、允许的 `PatternKind`、默认 `RuleStatus`、`meta` 校验。
3. 如有内置规则，返回 `source='builtin'`、`status='enabled'` 的 rules。
4. 如有自动学习，实现 `RuleLearningStrategy`，候选规则必须写入 `source='learned'`、`status='candidate'`。
5. 在 `analysis` 中新增 consumer，把 `RuleMatch` 解释为该语言的语义模型。
6. 在 MCP/CLI 层只暴露通用 CRUD/approve/list/learn 操作，不把语言语义硬编码进 tool router。

禁止：
- 在 `GenericRuleEngine` 中写入语言专属字符串判断。
- 在 DB schema 用 `CHECK(rule_kind IN (...))` 固定 rule kinds。
- 让 learned rules 默认生效。
- 把 C/C++ ownership 命名复用于其他语言。

## 3. Rule Kind Examples

| Language | Consumer | Example rule_kind | Example pattern |
|----------|----------|-------------------|-----------------|
| C/C++ | `CppOwnershipRules` | `free_fn`, `alloc_fn`, `owned_pattern`, `cleanup_fn` | `Curl_safefree`, `aprintf`, `data->state.aptr.*` |
| Rust | future safety consumer | `unsafe_boundary`, `drop_impl`, `trait_contract` | `transmute`, `ManuallyDrop::drop` |
| Python | future resource consumer | `context_manager`, `resource_factory`, `close_method` | `open`, `requests.Session`, `close` |
| Go | future resource/error consumer | `defer_cleanup`, `error_wrap`, `ctx_pass` | `defer resp.Body.Close`, `fmt.Errorf` |
| TypeScript | future framework consumer | `react_hook`, `state_setter`, `effect_cleanup` | `useEffect`, `useState`, `set[A-Z]*` |
| Generic | shared consumers | `deprecated`, `side_effect`, `error_propagate` | `@deprecated`, `log.Fatal` |

Use `language='*'` only for genuinely language-agnostic conventions. Prefer exact language registration when a rule depends on syntax, framework behavior, ownership semantics, or runtime conventions.

## 4. Pattern Guidance

Choose the narrowest `PatternKind` that represents the convention:

| PatternKind | Use when | Avoid when |
|-------------|----------|------------|
| `exact` | A concrete function/type/decorator name is known. | Matching families of generated names. |
| `prefix` | The convention is a stable prefix. | The prefix is too short or common. |
| `suffix` | The convention is a stable suffix. | The suffix appears in unrelated names. |
| `glob` | The convention is a simple family such as `set[A-Z]*`. | Regex-like precision is required. |
| `regex` | No simpler strategy can express the rule. | It would run over unbounded candidate sets. |

Regex rules must be cached or bounded by candidate count. Most user and builtin rules should be `exact`.

## 5. Metadata Guidance

`meta` is language-specific JSON. Keep it small and versioned by `meta_version`.

Recommended conventions:
- Store learned evidence under `meta.learned_from`.
- Limit learned evidence to representative samples, not every occurrence.
- Put consumer-specific confidence reasons in `meta.reason` or `meta.evidence_kind`.
- Bump `meta_version` when a consumer changes the expected shape.

Do not create a new table for evidence until real usage shows `meta.learned_from` is too large or too hard to query. If that happens, promote evidence into a dedicated table without changing the core matching API.

## 6. Status Semantics

Only `status='enabled'` rules participate in matching.

| Status | Meaning |
|--------|---------|
| `candidate` | Learned but not approved. Do not match. |
| `enabled` | Active. Match normally. |
| `disabled` | User temporarily disabled it. Do not match. |
| `rejected` | User rejected a candidate. Do not match or re-suggest without new evidence. |
| `deprecated` | Retained for audit/history. Do not match. |

Default status by source:
- `builtin`: `enabled`
- `user`: `enabled`
- `learned`: `candidate`

## 7. Verification Checklist

For each new language registry:

- Registry rejects unknown `rule_kind` for that language.
- Registry rejects unsupported `pattern_kind`.
- `GenericRuleEngine::match_pattern()` returns enabled exact-language rules.
- `GenericRuleEngine::match_pattern()` ignores `candidate`, `disabled`, `rejected`, and `deprecated`.
- `language='*'` fallback works only when intended.
- Consumer tests prove how `RuleMatch` is interpreted.
- Removing the registry leaves generic engine tests passing.
- The `domain_rules` crate still does not depend on `extraction`, `resolution`, `graph`, `lazy`, or `analysis`.

For learned rules:

- Candidates include evidence and confidence.
- Candidates are persisted as `source='learned'`, `status='candidate'`.
- Candidate rules do not affect analysis until approved.
- Approval changes status to `enabled` and makes subsequent matching return the rule.
