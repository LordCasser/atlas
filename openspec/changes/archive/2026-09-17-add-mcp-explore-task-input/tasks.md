## 1. 候选输入共享边界

- [x] 1.1 提取并复用候选 `InputRequest` 构造，锁定直接 MRTR 与 task 的 input id、schema、值顺序和标题完全一致
- [x] 1.2 提取严格候选响应解析，覆盖 accept、decline、cancel、畸形 action/内容、枚举外不可信选择器与原参数保留
- [x] 1.3 在创建 task 时原子捕获固定项目及 query snapshot 的原工具名/参数，保持项目切换安全

## 2. Explore task 状态机

- [x] 2.1 加入现代协议 + Tasks + form 三重门控；只有 Tasks 无 form 的 `explore` 保留现有 retry ticket，快速候选仍走直接 MRTR
- [x] 2.2 在 task 重放首次得到有效候选时通过 `TaskContext::request_input()` 进入 `input_required`，并在原硬截止内等待 `tasks/update`
- [x] 2.3 实现 accept 后固定项目精确调用及同 task 后续 retry；decline/cancel 完成原候选；畸形响应 failed；每个 task 最多一次候选输入
- [x] 2.4 覆盖 input_required wire、tasks/update 完成/失败、输入等待取消/过期、项目切换、无嵌套 task/第二句柄与普通 task 回归

## 3. 文档与验收

- [x] 3.1 更新路线图和变更日志，说明 `explore` task 的双能力门控、单次候选输入和 legacy/no-form 兼容边界
- [x] 3.2 运行严格 OpenSpec、格式检查、`atlas-mcp` 全特性测试、MCP CLI 测试和全目标 `-D warnings` Clippy
- [x] 3.3 完成独立审查并修复有效发现
- [x] 3.4 归档后运行全部 OpenSpec 严格校验与 `cargo clean`
