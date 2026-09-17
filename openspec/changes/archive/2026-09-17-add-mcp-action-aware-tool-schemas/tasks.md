## 1. 动作级 schema

- [x] 1.1 为 `domain_rules(add/delete)` 增加显式要求 action 存在的 `if`/`then` 必填条件
- [x] 1.2 为 `fp_dispatches(add/delete)` 增加条件必填，并以 `anyOf` 保持 delete 双标识兼容
- [x] 1.3 保持属性集合、顶层 required、默认 list、handler 运行时校验与其他工具 schema 不变

## 2. 契约回归

- [x] 2.1 精确锁定两个工具的根级 `allOf` 与嵌套动作条件
- [x] 2.2 覆盖缺少 action 不触发 mutation 必填、delete 至少一个标识且允许两者并存
- [x] 2.3 证明 Atlas 注册 schema 到 rmcp model/wire 无损，并保持 15 项目录名称、顺序与属性集合

## 3. 文档与验收

- [x] 3.1 更新路线图与变更日志，记录动作级机器可读输入契约和兼容边界
- [x] 3.2 在低内存约束下串行运行严格 OpenSpec、格式、schema 定向、`atlas-mcp`、MCP CLI 与 `-D warnings` Clippy
- [x] 3.3 完成独立审查并修复有效发现
- [x] 3.4 归档后运行全部 OpenSpec 严格校验、`cargo clean`、提交并推送
