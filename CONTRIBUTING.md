# Contributing

## Before opening a pull request

Keep changes focused and do not commit `data/`, `target/`, `dist/`, frontend `node_modules/`, cookies, tokens or local pairing files. Update public documentation when a command, environment variable, release asset or data path changes.

Run the checks relevant to the change:

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
python build.py --check
```

Frontend changes also require:

```bash
cd web
npm ci --ignore-scripts
npm run build
npm run test:smoke
```

## CI action version policy

第三方 GitHub Actions 在已提交的 workflow 中固定到完整 commit SHA，并在行尾保留对应的主版本注释（例如 `# v4`）。SHA 是实际执行的不可变版本，注释只用于阅读；不要把可变的分支名或版本标签提交到发布流程中。`.github/dependabot.yml` 已启用 GitHub Actions 的每周更新，以及 Cargo/npm 的定期更新，更新 PR 通过现有 CI 后即可合并。

这项规则服务于可复现构建和 Release 供应链安全，不是应用运行时的版本要求。Release job 具有写入制品的权限，因此必须优先保持固定；普通功能开发仍应优先关注行为正确性、测试和可维护性，不为形式审查引入无关重构。

For a release-affecting change, build the local platform package with `python build.py --portable` and confirm that the archive contains the executable, `static/`, `resources/aria2c*`, `resources/ffmpeg*`, any bundled Unix `resources/lib/` files, and the matching checksums. `ffprobe` is optional because the program falls back to FFmpeg for probing. Linux CI also builds a `core` archive without media runtimes for the dependency-aware installer. Do not push a tag unless the release assets are ready.

## Release checklist

1. `cargo fmt --all -- --check`（CI 第一关就会拦，但本地先跑可以省一整轮等待）。
2. 若改动了任何 `#[cfg(unix)]` 代码：本机是 Windows 时无法编译到这些代码。没有 WSL/交叉 C 工具链时 `cargo check --target x86_64-unknown-linux-gnu` 会因依赖的 cc build script 失败，此时依赖 CI quality gates 的 ubuntu `rust` job 兜底——务必走下述 dry-run 流程，不要直接打 tag。
3. **发版 dry-run**：提交并推送 main 后，用 `workflow_dispatch` 触发 Release workflow，`checkout_ref` 填 main、`publish=false`。全绿且 artifact 齐全后，再在同一 commit 上打 tag 正式发布。不要删 tag 重打；一个 tag 只对应一个 commit。
4. Linux 包的最低 glibc = Release 构建容器版本（当前 ubuntu:22.04，glibc ≤ 2.35）。升级容器镜像前要意识到这会同步抬高所有用户的兼容基线。

## Commit and review expectations

Use a short imperative subject, explain behavior changes and migration impact, and include manual reproduction steps for user-visible changes. Never include secrets or copied upstream binaries without updating `NOTICE.md` and `resources/README.md`.
