# Atlas 文档

本文档集只保留当前需要维护的少量文档。调研长文和重复设计草案已合并进下面文档，阶段日志单独保留。

## 阅读顺序

1. [需求规格](./01-requirements.md)
2. [架构约束](./02-architecture-constraints.md)
3. [当前架构实现](./03-current-architecture.md)
4. [架构与需求变更记录](./04-changes.md)
5. [未来架构演进](./05-roadmap.md)
6. [阶段日志](./06-phase-log.md)
7. [测试规范](./07-testing-spec.md)
8. [Trace Contract](./trace-contract.md)

## 维护规则

1. 改需求边界，更新 `01-requirements.md`。
2. 改模块边界、ID、持久化、解析/resolution/graph 规则，更新 `02-architecture-constraints.md`。
3. 改已经落地的代码结构、schema、CLI/MCP/analysis 能力，更新 `03-current-architecture.md`。
4. 改变方向或替代旧结论，更新 `04-changes.md`。
5. 未落地计划只写入 `05-roadmap.md`，不要混入当前实现。
6. 阶段性实施摘要写入 `06-phase-log.md`，不要塞回变更说明。
7. 改阶段验收、测试深度、feature 验证矩阵，更新 `07-testing-spec.md`。
8. 改 CLI/MCP trace JSON 字段、diagnostic code 或 capability 输出，更新 `trace-contract.md`。

根目录只保留 `README.md`。架构、需求、路线图和测试规范统一维护在 `docs/` 下。
