## 1. 持久 scope authority

- [x] 1.1 增加 `indexed_scope` → `DiscoveryConfig` 单一 loader，覆盖合法 include/exclude、显式 whole-project 与 unknown fields
- [x] 1.2 对 missing key 保留 legacy whole-project fallback 并记录 warning，对 malformed metadata fail closed
- [x] 1.3 让普通与 capability-aware detector 都使用持久 scope，不复制 glob 或 CLI 判定
- [x] 1.4 保持 scoped inventory 差集语义：scope 内 add/modify/delete/upgrade，scope 外缓存 facts 走既有清理

## 2. 可判别回归

- [x] 2.1 增加 detector 单测，覆盖 include、exclude、missing、malformed 与 capability-aware 分类
- [x] 2.2 增加 engine 状态转换回归，覆盖 scoped add/modify/delete、历史污染收敛与同等级 no-op
- [x] 2.3 增加真实 CLI binary → 持久 SQLite 回归，覆盖 scoped manifest → structural/full、scope 外隔离与 scope metadata 稳定
- [x] 2.4 保持 whole-project capability upgrade、path-alias、precision guard 与既有 pipeline equivalence 回归通过

## 3. 文档与验收

- [x] 3.1 更新路线图与变更日志，记录 sync 继承持久 Index scope 的边界
- [x] 3.2 串行运行严格 OpenSpec、格式、filesync、`atlas-cli` 与全目标 `-D warnings` Clippy 验收
- [x] 3.3 完成独立审查并修复有效发现
- [x] 3.4 归档后运行全部 OpenSpec 严格校验、`git diff --check`、`cargo clean`、提交并推送
