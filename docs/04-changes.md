# 架构与需求变更说明

本文只记录需求和架构方向上的关键变化：哪些旧结论被替代、为什么替代、当前应遵守什么方向。阶段性实施日志见 [阶段日志](./06-phase-log.md)。

## 1. 从 CodeGraph rewrite 改为 Rust-native Atlas

旧方向：

- 逐行迁移 CodeGraph。
- 追求 `.codegraph` schema 兼容。
- 尝试覆盖 23 种语言。
- 倾向复刻大型 `GenericExtractor + LangConfig`。

当前方向：

- Atlas 是 CodeGraph-inspired 的 Rust-native 本地代码知识图谱引擎。
- CodeGraph 只作为产品形态和经验参考。
- MVP 聚焦 7 种语言；Cangjie 暂时降级为显式 opt-in 的不完善支持语言。
- schema 为 Atlas 自有模型，保留 scopes、references、callsites、dataflow、CFG 和 trace 基础事实。
- extraction 使用 tree-sitter queries + LanguageAdapter。
- SQLite 是 source of truth，GraphSnapshot 是查询加速层。

## 2. Atlas 与 Corpus 边界拆分

新增过一个相关需求：大型多版本源码索引系统，面向 Linux 等多 release/tag 项目，参考 Elixir 的 Git tag/blob 去重和版本化源码查询。

最终决策：

- 不把多版本源码索引并入 Atlas 主体。
- Atlas 继续做单项目、单版本、本地 workspace graph。
- Corpus 如需立项，应作为独立应用。
- 可共享的是 tree-sitter 解析核心，而不是存储、ID、查询、Web/API 或 MCP 工具语义。

原因：

- Atlas 的核心身份是 project-relative path。
- Corpus 的核心身份是 Git blob + version/tag/path mapping。
- Atlas 查询图谱关系；Corpus 查询版本化源码、函数实现、first-seen、diff/timeline。
- 强行统一会污染两个产品的数据模型。

当前补充决策：

- 不立即拆分 crate。
- 不立即开启 Corpus 分支。
- 先基于当前架构完成变量来源追踪与调用路径查询端到端测试。
- trace 能力稳定后，先拆出包含语法解析、变量来源追踪和调用路径查询能力的 engine crate，以及交互用 CLI/MCP 层。
- 只有完成 engine/CLI/MCP 边界拆分后，后续演进才分叉为 Atlas 单仓库单版本索引和 Corpus 多版本源码索引。

## 3. 从 symbol-only graph 演进到 facts-first graph

旧方向容易把抽取结果直接压成最终 graph edge，导致 callsite、低置信度引用、局部数据流和代码路径缺少源码定位。

当前方向：

- references 是源码事实，必须长期保留。
- resolved facts 和 graph edges 分离。
- symbol graph、dataflow graph、CFG 分层建模。
- `symbol_edges` 只表达 symbol-level 关系。
- dataflow 使用 `DataNodeId -> DataNodeId`。
- CFG 独立记录函数内控制流。

## 4. 从 Resolver 创建边改为 GraphBuilder 创建边

旧方向中 Resolver 同时解析引用并创建 edges，职责混合，增量失效和 graph 重建都不清晰。

当前方向：

- Resolver 只更新 `"references"` 的 resolved fields。
- GraphBuilder 从 resolved references、callsites 和 structural facts 创建 symbol-level edges。
- Sync 修改或删除文件时先失效 resolved facts 和相关 edges，再重跑受影响链路。

## 5. 从符号级数据流改为专用 dataflow facts

旧方向曾把参数、返回值、赋值、读写等关系塞进 symbol edge，甚至需要伪造 symbol。

当前方向：

- Binding 和 DataNode 有独立 ID。
- DataFlowEdge 的 source/target 必须是 DataNode。
- callsite args、return、field access、local variable 都应能定位到数据流节点。
- 变量来源追踪与调用路径查询基于 dataflow facts，而不是 symbol edge。

## 6. 从自动扫描改为变量来源追踪与调用路径查询

旧方向一度把 P5 定义为自动漏洞传播扫描：

- 通过规则识别外部输入点、危险操作点和清洗函数。
- 全项目 forward propagation。
- 输出 finding、severity、path steps。

当前决策：

- 自动扫描引擎不进入当前产品线。
- 产品主线改为用户指定入口驱动的变量来源追踪器和调用路径查询器。
- 用户或 AI 先指定代码位置、目标变量、函数、调用点或代码模式，Atlas 负责找变量来源、调用者链路和可能调用路径。
- Atlas 不判断这些路径是否构成漏洞；AI 或用户基于 Atlas 返回的结构化证据继续分析。

Atlas 不包含污点分析（taint analysis），不维护对应规则、finding 或路径步骤表。

新增阶段门禁：

- 当前主线优先完成变量来源追踪与调用路径查询端到端测试。
- 端到端测试必须覆盖指定位置、变量来源、caller path、bounded CLI/MCP 输出；跨函数参数/返回追踪只有在 function summary 或等价事实接入后才能作为完成项验收。
- 只有当路径追踪作为产品能力稳定后，才把语法解析和 trace engine 抽出为可复用 crate。

## 7. 阶段实施记录

P0-P5 的阶段性实施内容单独维护在 [阶段日志](./06-phase-log.md)。`04-changes.md` 只保留方向性变更，不展开每个阶段的文件清单和测试细节。

## 8. 从内部能力假设改为用户可见能力边界

旧方向容易把“Atlas 支持某语言”理解成所有分析能力都支持该语言。

当前决策：

- 每种语言必须有显式 capability profile。
- 能力等级从 Level 0 到 Level 5 描述，不同语言可以停在不同等级。
- CLI、MCP、context 输出必须展示当前语言的 capability、limitations、unsupported features 和 confidence。
- 查询超出语言能力边界时返回 partial result + diagnostics，不能只给空结果，也不能把 best-effort 结果包装成精确结论。
- 能力等级升级必须由 fixture 和用户可见输出测试共同证明。
