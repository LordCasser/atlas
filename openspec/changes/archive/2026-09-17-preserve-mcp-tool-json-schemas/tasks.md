## 1. 完整 schema 合约

- [x] 1.1 将 `ToolInputSchema` 替换为完整 JSON object 封装，提供 raw object、现有 object schema 构造和对象读取/转交接口；通过 protocol 单元测试验证任意根级键值与 `inputSchema` object 序列化保持一致。
- [x] 1.2 将 15 项工具定义与所有 schema 消费测试迁移到新对象接口；运行 `cargo test -p atlas-mcp --test schema_validation --test symbol_selector_integration`，验证现有参数、必填字段和嵌套 `oneOf` 契约不变。

## 2. rmcp 无损转交与回归

- [x] 2.1 让 `to_rmcp_tool` 直接转交完整 schema object，不再投影关键字；增加合成高级 schema 回归，验证 `$schema`、`$defs`、根级组合/条件、`additionalProperties` 和未知关键字完全保留。
- [x] 2.2 增加真实 15 项工具目录 parity 回归，逐项比较 Atlas 与 rmcp schema object，并验证工具名称与顺序不变。

## 3. 文档同步

- [x] 3.1 更新 `docs/roadmap.md`，将完整 JSON Schema 2020-12 边界项标为完成，并记录 fidelity 测试证据。
- [x] 3.2 更新 `CHANGELOG.md` 的 Unreleased，记录当前 wire shape 不变及 `ToolInputSchema` Rust API 的源码级 breaking migration。

## 4. 验证与审查

- [x] 4.1 运行 `openspec validate preserve-mcp-tool-json-schemas --strict`、`cargo fmt --all -- --check`、`cargo test -p atlas-mcp --all-features`、`cargo test -p atlas-cli --features mcp` 与 `cargo clippy -p atlas-mcp --all-targets --all-features -- -D warnings`，全部退出码为 0。
- [x] 4.2 由独立 reviewer 审查最终 diff、schema fidelity、公共 API 迁移和验证证据；解决所有阻塞项后复跑受影响检查，并在完成归档后执行 `cargo clean`。