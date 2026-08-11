# BilibiliUIDBuildownloader

B 站 UID 视频监控与下载助手，后端使用 Rust/Axum，前端使用原生 JavaScript。

## 快速开始

```bash
cargo run
```

常用检查与构建命令：

```bash
cargo test --all-targets
python build.py --check
python build.py --portable
```

前端依赖与打包：

```bash
cd static/js
npm ci --ignore-scripts
npm run build
```

## 文档

- [代码规范](代码规范.md)
- [审查报告](AUDIT_REPORT.md)
- [问题归因与优化方案](docs/问题归因与优化方案.md)
- [AI Skill 使用说明](docs/skill.md)
- [部署安全说明](deploy/SECURITY.md)
- [内置资源清单与校验](resources/README.md)

## 部署

Linux、Termux 和 Caddy 示例位于 [`deploy/`](deploy/)。Windows 便携包使用 `python build.py --portable` 构建。

安全模式、配对、访问控制、代理和内置资源说明以 [`deploy/SECURITY.md`](deploy/SECURITY.md) 与 [`resources/README.md`](resources/README.md) 为准；不要把 LAN 模式或 `auth_bypass_ips` 当作公网安全边界。

## 发布与恢复最短流程

1. 升级或迁移前停止服务，并备份 `data/`（尤其是数据库、`security.toml` 和下载目录）。
2. 先运行 `cargo test --all-targets`、`npm run build`、`python build.py --check`；依赖审计、Bash 语法和 Windows release 以 CI 为准。
3. 迁移失败时保留原数据库和日志，不删除临时文件；从备份恢复后重新运行迁移，确认 `/api/ready` 正常再开放服务。
4. 回滚时停止新版本、恢复程序与 `data/` 备份，再启动旧版本；不要让新旧版本同时写同一数据库。
5. 服务重启后检查下载队列、直播恢复列表和 `recovery_state`；临时文件或 FLV 分段保留到确认恢复/清理完成。
6. 更新 `resources/` 中的内置资源时，同时更新 [`resources/README.md`](resources/README.md) 的版本、来源、许可证和 SHA-256，随后运行资源哈希检查。

运行时日志只记录操作类型、耗时、字节数、重试/恢复次数和队列长度等诊断字段；不要把 Cookie、token、完整查询参数或敏感绝对路径复制到工单中。

## 发布与恢复最短流程

1. 升级或迁移前停止服务，并备份 `data/`（尤其是数据库、`security.toml` 和下载目录）。
2. 先运行 `cargo test --all-targets`、`npm run build`、`python build.py --check`；依赖审计、Bash 语法和 Windows release 以 CI 为准。
3. 迁移失败时保留原数据库和日志，不删除临时文件；从备份恢复后重新运行迁移，确认 `/api/ready` 正常再开放服务。
4. 回滚时停止新版本、恢复程序与 `data/` 备份，再启动旧版本；不要让新旧版本同时写同一数据库。
5. 服务重启后检查下载队列、直播恢复列表和 `recovery_state`；临时文件或 FLV 分段保留到确认恢复/清理完成。
6. 更新 `resources/` 中的内置资源时，同时更新 [`resources/README.md`](resources/README.md) 的版本、来源、许可证和 SHA-256，随后运行资源哈希检查。

运行时日志只记录操作类型、耗时、字节数、重试/恢复次数和队列长度等诊断字段；不要把 Cookie、token、完整查询参数或敏感绝对路径复制到工单中。
