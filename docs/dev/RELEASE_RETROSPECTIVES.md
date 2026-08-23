# 发版复盘

记录每次发版过程中踩过的坑、根因，以及**下个版本仍可能再犯的未根治风险**。发新版前应通读最新一篇。

---

## v2.0.0-alpha.10（8 轮 CI，前 7 轮失败，累计修 10 个问题）

### 逐轮错误清单

#### 第 1 轮（5 个 job 同时失败）

1. **Unix 平台编译失败（E0599）——macOS ×2、Termux、E2E 全挂**
   - 错误：`no function named 'from_mode' found for struct 'Permissions'`
   - 根因：在 `ensure_control_socket_parent` 里用了 `PermissionsExt` trait 的方法，但只导入了 `MetadataExt`
   - 为什么本地没发现：Windows 上 `#[cfg(unix)]` 代码根本不编译，clippy/测试全绿
   - 教训：改 cfg-gated 平台代码时，本机的"全部通过"没有意义
2. **cargo fmt --check 失败**：两处换行风格与 rustfmt 不一致；quality gate 直接拦下

#### 第 2 轮

3. **FFmpeg 容器 job：`sh: Bad substitution`（exit 2）**
   - 根因：ubuntu 容器里 GitHub Actions 默认 shell 是 dash，不认 bash 特有的 `${VAR//./-}`
   - 修复：job 级强制 `defaults.run.shell: bash`

#### 第 3 轮

4. **libass configure 找不到 fribidi**
   - 根因：meson 在该容器里把 `.pc` 和库装到了 `/opt/ffdeps/lib64/`（自作主张选了 lib64），不在 `PKG_CONFIG_PATH` 也不在 `-L` 链接路径里
   - 修复：三个 meson 构建显式加 `--libdir=lib`（比拼路径更稳）

#### 第 4 轮

5. **FFmpeg 链接失败：`undefined reference to ff_udp_get_last_recv_addr`**
   - 根因：GnuTLS 后端的 `tls_gnutls.c` 引用 udp 协议的符号，但 configure 没启用 udp；win64 的 schannel 后端不走这段所以从未暴露
   - 修复：configure 加 `--enable-protocol=udp`

#### 第 5 轮

6. **Linux 打包 job 起不来：python 报 `GLIBC_2.38 not found`**
   - 根因：`actions/setup-python` 的独立版 CPython 是在新 glibc 上构建的，22.04 容器里连解释器都启动不了——和我们要修的用户问题一模一样
   - 修复：去掉 setup-python，apt 装 python3（3.10），build.py 加 tomllib 回退

#### 第 6 轮

7. **rustup 拒绝安装：`$HOME differs from euid-obtained home directory`**
   - 根因：容器默认 `HOME=/github/home`，但 root 的 passwd home 是 `/root`
   - 修复：job env 显式 `HOME: /root`

#### 第 7 轮

8. **打包时 ldd 检出 ffmpeg.bin 缺动态库，但报错没说缺哪个**
   - 根因：自建 FFmpeg 动态链接 Fontconfig/GnuTLS，打包容器只被 aria2 顺带装了 gnutls 系，没人带 fontconfig
   - 修复：补装 `libfontconfig1`；build.py 报错改成列出具体库名

第 8 轮全绿。

### 非 CI 问题

- 推送被分支保护规则拦截（"must use PR"）——以仓库管理员身份 bypass。如果以后有协作者，这套直推 main + 移 tag 的流程走不通
- Dependabot 4 条告警：vite 5→6 大版本升级解决，属 dev 工具链，不影响产物

---

## v2.0.0-alpha.11（5 次 run：dry-run ×2 + 发布尝试 ×3）

本次首次执行"dry-run 先行、绿了再打 tag"的新流程，dry-run 机制本身两次拦截失败于打 tag 之前；但发布阶段仍踩了三个新坑。

### 错误清单

1. **linux-artifacts 容器 dash 不认 pipefail（39s 失败）**
   - 根因：新加的 FFmpeg 动态依赖校验步骤写了 `set -euo pipefail` 但没指定 shell——正是 alpha.10 教训 #3 的原样复犯
   - 修复：linux-artifacts job 级 `defaults.run.shell: bash`（比逐步骤补更稳）
2. **Publish GitHub Release 被静默跳过**
   - 根因：publish 条件写成 `inputs.publish != false`，tag push 事件下该 input 为空，GitHub 表达式做数值强转后 `null(0) != false(0)` 为假 → 正式发版的 Publish 也被跳掉
   - 教训：GH 表达式对空值有隐式类型强转，条件判断"开关未设置时默认为真"要用 `(github.event_name == 'push' || inputs.publish == true)` 这类显式写法
   - 为什么 dry-run 没拦住：dry-run 里 publish 步骤本来就不跑，这类"只在正式路径生效"的逻辑是 dry-run 的盲区
3. **Verify release files 失败：Windows portable 包凭空消失**
   - 根因：build.py 新加的"清扫 dist/ 旧产物"逻辑会删掉同一 runner 上先打好的包——Windows job 先 portable 后 core，第二次组装时清理误删第一次的归档；上传步骤因 core zip 存在而通过，拖到 publish 校验才爆出来
   - 修复：清理只删不含 `-v{当前版本}` 的文件；本地连续组装 portable + core 复现并验证
4. **修复没进被构建的 commit（流程性错误）**
   - dispatch 发布走的是 tag 指向的代码，修在 main 上不生效，白白烧了一轮构建
   - 教训：发版序列一旦开始，workflow/build 脚本的修复必须先进 commit、再把 tag 移过去重走；移 tag 前确认没有任何已发布的资产（本例失败轮次从未产出可下载资产，移动安全）

### 本次流程收益

- dry-run 在打 tag 前拦住了问题 1 和 3，tag 最终只钉在一个 fully-verified 的 commit 上（对比 alpha.10 重移 6 次）
- FFmpeg ldd 清单 + 打包容器按单复检落地，缺库时报具体库名

---

## ⚠️ 未根治、可能复发的风险

| # | 风险点 | 为什么会复发 |
|---|---|---|
| 1 | **unix-only Rust 代码本地验证盲区** | Windows 开发机编译不到 `#[cfg(unix)]` 代码，且没有 WSL 时 `cargo check --target x86_64-unknown-linux-gnu` 会因依赖的 cc build script 失败。目前靠 CI quality gates 的 ubuntu rust job 兜底——所以 dry-run 流程不能省 |
| 2 | **setup-python / setup-node 等 Action 与老容器的兼容性** | actions/* 的产物跟随最新 glibc 构建，22.04 容器未来还可能踩类似坑。这是容器化策略的固有税 |
| 3 | **自建 FFmpeg 的动态依赖是隐式契约** | 已用 ldd 清单缓解，但将来 FFmpeg 多开一个能力仍需同步更新打包容器的 apt 清单 |
| 4 | **FFmpeg 编译时长无缓存** | x264/harfbuzz/libass 每次发版从零编译（约 20 分钟），ccache/meson 缓存没配 |
| 5 | **cargo fmt/clippy 门禁依赖提交者自觉本地检查** | CI 能拦住，但失败成本是一整轮等待 |
| 6 | **workflow/build 脚本修复与 tag 的同步纪律** | 发版中途修 CI 必须移 tag；若届时 Release 页已有资产则不能再移，只能换版本号 |
| 7 | **直推 main + bypass 分支保护** | 有协作者后走不通，需要提前设计 PR 流程 |

## 发版前 checklist（详见 CONTRIBUTING.md）

1. `cargo fmt --all -- --check`
2. 改过 `#[cfg(unix)]` 代码且本机无 Linux 工具链时：确认理解 CI ubuntu job 是唯一防线，务必走 dry-run
3. 提交推送 main 后先用 workflow_dispatch dry-run（`checkout_ref`=main、`publish=false`），全绿再在同一 commit 打 tag
4. 发版序列中的任何修复：先落 main commit → 确认无已发布资产 → 移 tag 重走
5. 记住 Linux 包最低 glibc = CI 容器版本（当前 ubuntu:22.04 / glibc ≤ 2.35），升容器 = 抬高所有用户的兼容基线
