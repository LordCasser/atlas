## 1. 确认状态与表单边界

- [x] 1.1 实现两个 delete 调用形状识别、现代协议 + form + 活动项目门控，以及目标/项目可见的 required boolean 确认表单
- [x] 1.2 实现最多 128 项、300 秒 TTL 的 UUID opaque state 容器，保存完整参数与固定项目 router，并覆盖过期清理和满载行为
- [x] 1.3 实现原子一次性兑换、工具/完整参数匹配、畸形/错误流程/并发重放 `invalid_params`，确保失败状态不恢复

## 2. 核心复用与兼容回归

- [x] 2.1 接入 `prepare_tool_call()` 和 execution router override；accept+true 在固定项目调用一次现有 domain rule / FP delete 核心
- [x] 2.2 实现 decline/cancel/accept+false 的完整非错误未删除结果，验证不删除规则、annotation、物化边或图状态
- [x] 2.3 覆盖项目切换、legacy/no-form、无项目、非 delete、畸形目标、核心错误、其他 MRTR/Tasks 与直接 `ToolRouter::call_tool()` 回归
- [x] 2.4 覆盖真实 rmcp wire round trip、固定 input id/schema/message、opaque requestState、一次性兑换和并发重放

## 3. 文档与验收

- [x] 3.1 更新路线图和变更日志，明确两个 overlay delete 的现代交互确认、固定项目、一次性状态与非授权边界
- [x] 3.2 运行严格 OpenSpec、格式检查、`atlas-mcp` 全特性测试、MCP CLI 测试和全目标 `-D warnings` Clippy
- [x] 3.3 完成独立审查并修复有效发现
- [x] 3.4 归档后运行全部 OpenSpec 严格校验与 `cargo clean`
