# 云端发布架构

仓库的云端职责按入口拆分，避免普通 CI、GitHub Pages 和 Release 打包互相阻塞：

| 工作流 | 入口 | 责任 | 输出 |
| --- | --- | --- | --- |
| [CI](../../.github/workflows/ci.yml) | `main` 推送、Pull Request、手动运行 | Rust、前端、依赖、安全策略和 Windows 构建质量门禁 | 检查结果，不发布归档 |
| [Release](../../.github/workflows/release.yml) | `v*` Tag，或手动指定已有 Tag | 多平台 portable/core/Termux 打包、清单生成、Release 上传和资产校验 | GitHub Release、归档、归档 `.sha256`、`latest.json` 与 `latest.json.sha256` |
| [Pages](../../.github/workflows/pages.yml) | `main` 推送、手动运行 | 只部署 `docs/` 静态站点 | GitHub Pages |

## 发布规则

1. 版本号以 `Cargo.toml` 的 `package.version` 为准，Tag 使用 `v${version}`。
2. 推送 Tag 后，Release 工作流从该 Tag checkout，所有构建和下载资产都使用同一版本。
3. Alpha、Beta、RC 自动标记为预发布；Release 创建后还会逐个核对资产数量、文件大小和本地 SHA-256 清单。
4. Release 工作流不会被普通 CI 的并发取消；同一个 Tag 的重复运行会排队，避免半成品覆盖。
5. 依赖审计由 `main`/Pull Request 的 CI 门禁执行；Release 复用 Rust、前端和 Windows 质量检查，并用最终资产校验守住发布边界，避免重复下载审计 action 造成发布阻塞。
6. 需要补发已有 Tag 时，在 Actions 中手动运行 Release，并填写 Tag，例如 `v2.0.0-alpha.5`。工作流会在同一 Tag 上创建或更新 Release，不需要移动 Tag。

## 下载入口

用户只从 [GitHub Releases](https://github.com/Wong0728/bulibuli/releases) 下载归档；源码、Pages 静态站点和 Actions 临时 artifact 不作为正式发布入口。每个归档都带有同名 `.sha256` 校验资产，三平台安装器与应用内更新都在核对 SHA-256 之后才落盘。

清单本身的校验文件：Release 工作流除归档外写出 `latest.json`（版本号加资产名、下载地址与 SHA-256）和它的同名文件 `latest.json.sha256`，两者随归档一起上传，发布前的 `sha256sum -c` 循环会把 `latest.json.sha256` 与各归档的 `.sha256` 一并核对。需要明确边界：`latest.json.sha256` 守住的是清单与归档在发布产物内部的自洽，安装器和应用内更新都从同一个 GitHub 通道取回清单，不会再用该文件验证清单自身（同通道的校验值无法自证），所以更新清单仍然没有独立签名。威胁模型与用户侧缓解措施见 [已知问题与暂不支持清单](KNOWN_ISSUES.md)。

各安装器的版本解析口径不同：Windows 的 `install.ps1` 通过 `releases/latest/download/latest.json` 取版本，该地址跳过 draft 与预发布，因此只有正式版 Release 会被 Windows 用户装到；Linux 与 Termux 的 `install.sh` 不读 `latest.json`，直接查询 Releases API，默认取列表中最新一条 Release（含预发布），可用 `BULIBULI_STABLE_ONLY=1` 收敛到正式版。发布预发布版本时据此判断影响面。
