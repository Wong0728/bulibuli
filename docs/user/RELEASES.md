# 云端发布架构

仓库的云端职责按入口拆分，避免普通 CI、GitHub Pages 和 Release 打包互相阻塞：

| 工作流 | 入口 | 责任 | 输出 |
| --- | --- | --- | --- |
| [CI](../../.github/workflows/ci.yml) | `main` 推送、Pull Request、手动运行 | Rust、前端、依赖、安全策略和 Windows 构建质量门禁 | 检查结果，不发布归档 |
| [Release](../../.github/workflows/release.yml) | `v*` Tag，或手动指定已有 Tag | 多平台 portable/core/Termux 打包、清单生成、Release 上传和资产校验 | GitHub Release、归档、`.sha256`、`latest.json` |
| [Pages](../../.github/workflows/pages.yml) | `main` 推送、手动运行 | 只部署 `docs/` 静态站点 | GitHub Pages |

## 发布规则

1. 版本号以 `Cargo.toml` 的 `package.version` 为准，Tag 使用 `v${version}`。
2. 推送 Tag 后，Release 工作流从该 Tag checkout，所有构建和下载资产都使用同一版本。
3. Alpha、Beta、RC 自动标记为预发布；Release 创建后还会逐个核对资产数量、文件大小和本地 SHA-256 清单。
4. Release 工作流不会被普通 CI 的并发取消；同一个 Tag 的重复运行会排队，避免半成品覆盖。
5. 依赖审计由 `main`/Pull Request 的 CI 门禁执行；Release 复用 Rust、前端和 Windows 质量检查，并用最终资产校验守住发布边界，避免重复下载审计 action 造成发布阻塞。
6. 需要补发已有 Tag 时，在 Actions 中手动运行 Release，并填写 Tag，例如 `v2.0.0-alpha.5`。工作流会在同一 Tag 上创建或更新 Release，不需要移动 Tag。

## 下载入口

用户只从 [GitHub Releases](https://github.com/Wong0728/bulibuli/releases) 下载归档；源码、Pages 静态站点和 Actions 临时 artifact 不作为正式发布入口。安装器使用 Release 中的 `latest.json` 和同名校验文件。
