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
