## 1. 协议适配与回归

- [x] 1.1 在 MCP transport 适配层实现版本感知的工具目录结果：`2026-07-28+` 返回 300,000 ms 正 TTL 与 `public` cache scope，旧版或未知版本省略字段；通过针对性单元测试验证版本分支。
- [x] 1.2 增加 rmcp JSON 序列化回归，验证现代 wire shape、legacy 字段省略、legacy `resultType` 处理以及两种版本的工具数组完全一致。

## 2. 文档同步

- [x] 2.1 更新 `docs/roadmap.md`，将 MCP 工具目录缓存项标为完成，并记录版本分支与测试证据；校对文档与实现常量一致。
- [x] 2.2 更新 `CHANGELOG.md` 的 Unreleased，记录现代协议新增标准缓存提示且旧协议契约不变。

## 3. 验证与审查

- [ ] 3.1 运行 `openspec validate cache-mcp-tool-catalog --strict`、`cargo fmt --all -- --check`、`cargo test -p atlas-mcp --all-features`、`cargo test -p atlas-cli --features mcp` 与 `cargo clippy -p atlas-mcp --all-targets --all-features -- -D warnings`，全部退出码为 0。
- [ ] 3.2 由独立 reviewer 审查最终 diff、协议版本边界和验证证据；修复所有阻塞项后复跑受影响检查。