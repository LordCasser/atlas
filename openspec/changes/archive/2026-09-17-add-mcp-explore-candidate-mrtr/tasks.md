## 1. 适配层 MRTR 逻辑

- [x] 1.1 实现协议版本与表单 elicitation 双重门控，并用单元测试验证现代交互、旧协议、缺失能力和仅 URL 能力四类结果
- [x] 1.2 从现有 `explore` 歧义完整结果构造有界候选表单 `InputRequiredResult`，并用 wire-shape 测试验证选择值、可读标题及不包含 `requestState`
- [x] 1.3 解析候选 `inputResponses`，实现接受时精确改写 `symbol`、拒绝/取消时完整结果回退、畸形响应和非空 `requestState` 的 `invalid_params`，并覆盖各分支

## 2. 服务集成与兼容回归

- [x] 2.1 将候选 MRTR 接入 `ServerHandler::call_tool()`，并验证现代交互客户端仅对真实歧义 `explore` 返回 `input_required`
- [x] 2.2 验证接受选择后保留原 `explore` 其余参数并返回完整结果，拒绝/取消不循环，确定性查询与其他工具保持单轮完成
- [x] 2.3 验证旧协议/非交互客户端仍收到现有候选 JSON，15 项工具目录、顺序、`explore.inputSchema` 和 `ToolRouter::call_tool()` 结果保持不变

## 3. 文档与验收

- [x] 3.1 更新路线图和变更日志，准确记录本切片只覆盖 `explore` 候选选择且不使用 `requestState`
- [x] 3.2 运行 `openspec validate add-mcp-explore-candidate-mrtr --strict`、格式检查、`atlas-mcp` 全特性测试、MCP CLI 测试和 `-D warnings` Clippy，全部通过
- [x] 3.3 完成独立代码审查，修复有效发现后归档该 OpenSpec，再运行 `openspec validate --all --strict` 与 `cargo clean` 并确认构建产物已回收