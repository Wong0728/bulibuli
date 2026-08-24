# 安全访问与 Caddy 部署

程序首次初始化会在本机终端显示一次性配对码。以后需要新增设备时，只能在服务主机上执行：

```text
bulibuli ctl pair
```

默认 `local` 模式只监听 `127.0.0.1`。可信局域网可显式切换为 `lan`；公网必须配置 HTTPS 反向代理：

```text
bulibuli ctl mode lan
bulibuli ctl mode proxy downloads.example.com
```

模式切换后重启服务。`proxy` 默认从配置端口起连续尝试绑定，端口被占用会回退到后续端口；请以启动日志或 `data/actual_port.txt` 中的实际端口更新反代配置。复制 `deploy/caddy/Caddyfile.example`，替换域名并让 Caddy 加载配置。示例会覆盖 `X-Forwarded-For`、代理 Socket.IO，并发送 HSTS、CSP、`nosniff`、`DENY` 和 `no-referrer`；默认不记录包含完整查询参数的访问日志。

常用本机控制（需服务已运行；先执行 `bulibuli ctl ai on` 启用 AI Skill 模式后，以下命令全部可用）：

```text
bulibuli ctl sessions
bulibuli ctl revoke <session-id|all>
bulibuli ctl access deny 203.0.113.0/24
bulibuli ctl access allow 198.51.100.8 --minutes 30
bulibuli ctl access list
bulibuli ctl geo cn on
bulibuli ctl geo db /absolute/path/GeoLite2-Country.mmdb
bulibuli ctl geo db remove
bulibuli ctl trust aria2 http://127.0.0.1:6800/jsonrpc
bulibuli ctl trust ffmpeg /absolute/path/ffmpeg
```

### 内置 GeoIP 数据库（开箱即用）

程序自 v2.0.0 起在 `resources/geo/GeoLite2-Country.mmdb` 内置一份 DB-IP 国家库
（CC BY 4.0，IPv4 版本）。启动时会自动发现并在日志中打印 `已发现内置 GeoIP 数据库`。
因此执行 `geo cn on` 即可启用大陆 IP 配对限制，**无需先执行 `geo db <path>`**。

- `geo db <path>` 可显式指定其他数据库（如带 IPv6 数据的版本），覆盖内置数据库。
- `geo db remove` 清除显式配置，回退到内置数据库。
- `config` 命令的输出包含 `effective_geo_db` 字段，表示当前实际生效的数据库路径。
- 内置数据库仅含 IPv4。Local 模式默认监听 `127.0.0.1`，不受影响；LAN/proxy 模式下
  若有 IPv6 客户端配对，会被以"无法判断网络区域"拒绝。需要 IPv6 支持时请用
  `geo db <path>` 指定同时包含 IPv4+IPv6 的 mmdb 数据库。
- 数据库每月由 DB-IP 发布新版本，更新方式见 `resources/README.md`。

`proxy` 默认从主端口开始绑定，若端口被占用会回退到后续端口；请以启动日志或 `data/actual_port.txt` 中的实际端口更新 Caddy 的反代目标。`lan` 使用 HTTP，不具备链路加密，只适合可信局域网。应用内 IP/CIDR 规则是请求级访问控制，不能代替主机防火墙，也不能保护家庭公网带宽免受 DDoS。

### LAN 模式的 HTTP 明文传输风险

`lan` 模式全程使用明文 HTTP：登录会话 Cookie、CSRF Token、B 站 Cookie 状态和所有下载/历史数据都会以未加密形式在局域网中传输，同一网络内的任何设备都可以被动嗅探或通过 ARP 欺劫等中间人手段窃取、篡改这些流量。因此：

- 只在完全可信的局域网（家庭/个人网络）启用 `lan` 模式；公司、公共 Wi-Fi 或有不可信设备接入的网络请改用 `proxy` + HTTPS。
- 配合主机防火墙限制监听端口的来源网段；应用内访问规则不能替代防火墙。
- 会话空闲 TTL 已按模式区分：LAN 模式下空闲会话 7 天过期（Local/Proxy 为 30 天），以缩短明文 Cookie 被嗅探后的可用窗口；绝对有效期仍为 90 天。
- 需要远程或跨网段访问时，优先选择 `proxy` 模式并部署 HTTPS 反向代理（见上文 Caddy 示例）。

Cloudflare 应设为 DNS-only（灰云）。这种模式不会提供 Cloudflare WAF/DDoS 代理保护；若源站有 IPv6 DNS 记录，源 IPv6 也会公开。应同时配置主机防火墙，只开放 Caddy 的 HTTPS 端口。
如确需在受控部署中跳过会话认证，只配置 `security.toml` 的 `auth_bypass_ips` 明确单个客户端 IP。该字段不接受 CIDR，默认为空；不要填入 `0.0.0.0`、`::` 等未指定地址，也不要把它理解为可信网络。服务启动时会对非空配置打印高风险告警；反向代理场景还必须确认 `X-Forwarded-For` 的来源可信。

SQLite 数据库默认不做静态加密，数据目录依赖操作系统的用户权限或磁盘加密保护。不要把 `data/` 放在共享目录、公共同步盘或可被其他账号读取的位置；其中可能包含历史记录、会话、Cookie 和下载路径。备份和迁移副本也应使用同等权限保护。

### 关于 install.sh 的 SHA-256 校验（信息级）

`deploy/linux/install.sh`（以及 Termux 安装器）对下载归档执行的 SHA-256 校验**仅用于防范传输损坏**，不能替代数字签名校验，也不能证明发布资产未被上游供应链污染。如需更强的完整性保证，请从 GitHub Releases 页面核对官方公布的 `.sha256` 文件，并关注后续的签名发布方案。

内置 aria2c 使用 `--stop-with-process` 绑定主进程，Windows 还有 Job Object，Linux 还有父进程死亡信号；正常退出先使用 RPC，超时才强制 kill。macOS、Android/Termux 的强制结束、会话恢复和文件锁释放仍必须在目标设备上实测，不能仅由 Windows/Linux 分支推断。

### 升级、备份与恢复

升级前停止服务并完整备份 `data/`。应用内 `POST /api/backup` 只是 SQLite 数据库快照，不包含密钥、`security.toml`、`startup_state.json` 或下载目录；`POST /api/backup/full` 才生成带清单的完整恢复目录。完整恢复目录包含数据库快照、密钥材料、设置状态、日志和下载文件，必须按 `BACKUP-MANIFEST.json` 恢复，并保护其权限。迁移失败时保留原数据库、迁移日志和下载目录，先恢复备份再重试，不要手工删除迁移表。回滚必须同时恢复程序版本和数据库备份，避免新旧 schema 混用。

服务重启后检查下载队列、直播恢复列表和直播合并任务；`recovery_state` 为 `segments_pending` 或 `output_missing_recoverable` 时，先通过应用提供的恢复/重试入口处理，确认最终文件和数据库状态一致后再清理分段。资源替换必须同步更新 `resources/README.md` 的来源、许可证和 SHA-256，并重新运行构建检查。
