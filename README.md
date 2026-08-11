# 补哩补哩 bulibuli

> 下架之前，先下为敬。

补哩补哩是一个基于 Rust/Axum 的 B 站视频监控、补档下载与直播录制工具。当前 `main` 是 Rust v2 Alpha 主线；Python v1 仅保留在 `v1-python` 分支、`v1.0` 标签和旧历史中，不属于当前产品。

主线只包含本项目的 Rust 服务、前端、部署脚本、运行资源、测试和文档。外部项目、参考代码和示例目录不属于主线，也不会进入 Release 归档。

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

## 发布版本

`v2.0.0-alpha.1` 是 Rust v2 Alpha 预发布版本，包含 4 个平台归档及其 `.sha256` 校验文件：Windows x86_64、Linux x86_64、macOS Intel 和 macOS Apple Silicon。Windows 便携包可直接解压运行，Linux 包包含一键安装脚本。二进制 Release 归档只包含运行所需文件，不包含测试、外部项目或本地数据；GitHub 自动生成的 Source archive 对应清理后的第一方主线。

Linux 一键安装：

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/v2.0.0-alpha.1/deploy/linux/install.sh | bash
```

Linux 安装脚本支持 `install`、`run`、`service`、`unservice` 和 `status`。Termux 请使用：

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/v2.0.0-alpha.1/deploy/termux/install.sh | bash
```

Termux 首次运行会使用本机 Rust 编译，并通过 `pkg` 安装 aria2、FFmpeg 和 Termux 所需工具；开机自启还需要 Termux:Boot 应用。

## Hero 页面

项目的独立静态 Hero 展示页：[打开 Hero HTML](docs/hero/bulibuli_hero_fully_editable_v2.html)。该页面不参与主程序构建，也不进入二进制 Release；下载 HTML 后可直接用浏览器打开。

## 文档

- [代码规范](代码规范.md)
- [审查报告](AUDIT_REPORT.md)
- [问题归因与优化方案](docs/问题归因与优化方案.md)
- [AI Skill 使用说明](docs/skill.md)
- [部署安全说明](deploy/SECURITY.md)
- [内置资源清单与校验](resources/README.md)

安全模式、配对、访问控制、代理和内置资源说明以 [`deploy/SECURITY.md`](deploy/SECURITY.md) 与 [`resources/README.md`](resources/README.md) 为准；不要把 LAN 模式或 `auth_bypass_ips` 当作公网安全边界。

## 发布与恢复

1. 升级或迁移前停止服务，并备份 `data/`（尤其是数据库、`security.toml` 和下载目录）。
2. 先运行 `cargo test --all-targets`、`npm run build`、`python build.py --check`；依赖审计、Shell 和跨平台 release 以 CI 为准。
3. 迁移失败时保留原数据库和日志，不删除临时文件；从备份恢复后重新运行迁移，确认 `/api/ready` 正常再开放服务。
4. 回滚时停止新版本、恢复程序与 `data/` 备份，再启动旧版本；不要让新旧版本同时写同一数据库。
5. 服务重启后检查下载队列、直播恢复列表和 `recovery_state`；临时文件或 FLV 分段保留到确认恢复/清理完成。
6. 更新 `resources/` 中的内置资源时，同时更新 [`resources/README.md`](resources/README.md) 的版本、来源、许可证和 SHA-256。

运行时日志只记录操作类型、耗时、字节数、重试/恢复次数和队列长度等诊断字段；不要把 Cookie、token、完整查询参数或敏感绝对路径复制到工单中。
