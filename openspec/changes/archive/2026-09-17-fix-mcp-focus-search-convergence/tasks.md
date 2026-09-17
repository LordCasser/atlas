## 1. 收敛协调

- [x] 1.1 提取 scoped search 执行与 deferred Focus 入队协调，识别 partial + deferred + zero-pending 竞态
- [x] 1.2 对该竞态复用同一请求至多重读一次，并让第二次结果重新走现有 pending/terminal 判定
- [x] 1.3 保持真实 pending、失败、预算/发现边界、排序、limit、schema 与其他工具行为不变

## 2. 可判别回归

- [x] 2.1 用真实 inventory、structural materializer 和 search service 构造“首次 partial 后 deferred 已完成”的确定性回归
- [x] 2.2 覆盖协调后 complete 的结果、coverage、warnings/gaps/retry 一致性，以及第二次仍 partial 的有界语义
- [x] 2.3 重复运行现有 cold-search 恢复测试，确认不再产生无 retry marker 的 stale partial

## 3. 文档与验收

- [x] 3.1 更新路线图与变更日志，记录 cold-search execute→enqueue 竞态和单次协调边界
- [x] 3.2 串行运行严格 OpenSpec、格式、`atlas-mcp` 全特性、MCP CLI 与全目标 `-D warnings` Clippy
- [x] 3.3 完成独立审查并修复有效发现
- [x] 3.4 归档后运行全部 OpenSpec 严格校验、`cargo clean`、提交并推送
