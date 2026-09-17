## 1. project-path MRTR 适配逻辑

- [x] 1.1 实现 `project(open)` 缺失/空路径、SEP-2322 日期版本和显式表单能力三重门控，并覆盖旧协议、无能力、其他 action、已有路径及错误类型路径
- [x] 1.2 构造 required `project_path` 字符串表单，验证长度边界、描述、`input_required` wire shape 与省略 `requestState`
- [x] 1.3 解析独立路径 `inputResponses`，实现 accept 注入、decline/cancel 回退及缺失/额外/未知/空值/state 的 `invalid_params`

## 2. 服务集成与核心复用

- [x] 2.1 将路径 preflight 接入 `ServerHandler::call_tool()`，确保初始缺失路径不执行核心，重试后最多执行一次且不形成循环
- [x] 2.2 使用临时目录验证接受路径后由真实 `handle_open_project()` 激活 canonical 项目并创建/打开现有 `.atlas/atlas.db`
- [x] 2.3 验证 legacy/no-form、decline/cancel、错误路径、project status/files、explore 候选流程、其他工具、15 项目录、project schema 与 `ToolRouter::call_tool()` 直接结果不变

## 3. 文档与验收

- [x] 3.1 更新路线图和变更日志，明确第二个 MRTR 增量仅补全显式 `project(open)` 的缺失路径且不使用 `requestState`
- [x] 3.2 运行严格 OpenSpec、格式检查、`atlas-mcp` 全特性测试、MCP CLI 测试和全目标 `-D warnings` Clippy
- [x] 3.3 完成独立审查并修复有效发现，归档后运行全部 OpenSpec 严格校验与 `cargo clean`