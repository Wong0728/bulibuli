# 安全访问与 Caddy 部署

程序首次初始化会在本机终端显示一次性配对码。以后需要新增设备时，只能在服务主机上执行：

```text
bilibili-uid-buildownloader ctl pair
```

默认 `local` 模式只监听 `127.0.0.1`。可信局域网可显式切换为 `lan`；公网必须配置 HTTPS 反向代理：

```text
bilibili-uid-buildownloader ctl mode lan
bilibili-uid-buildownloader ctl mode proxy downloads.example.com
```

模式切换后重启服务。`proxy` 固定监听 `127.0.0.1:5000`，端口被占用时直接启动失败。复制 `deploy/caddy/Caddyfile.example`，替换域名并让 Caddy 加载配置。示例会覆盖 `X-Forwarded-For`、代理 Socket.IO，并发送 HSTS、CSP、`nosniff`、`DENY` 和 `no-referrer`；默认不记录包含完整查询参数的访问日志。

常用本机控制：

```text
bilibili-uid-buildownloader ctl sessions
bilibili-uid-buildownloader ctl revoke <session-id|all>
bilibili-uid-buildownloader ctl access deny 203.0.113.0/24
bilibili-uid-buildownloader ctl access allow 198.51.100.8 --minutes 30
bilibili-uid-buildownloader ctl access list
bilibili-uid-buildownloader ctl geo cn on
bilibili-uid-buildownloader ctl geo db /absolute/path/GeoLite2-Country.mmdb
bilibili-uid-buildownloader ctl geo db remove
bilibili-uid-buildownloader ctl trust aria2 http://127.0.0.1:6800/jsonrpc
bilibili-uid-buildownloader ctl trust ffmpeg /absolute/path/ffmpeg
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

`lan` 使用 HTTP，不具备链路加密，只适合可信局域网。应用内 IP/CIDR 规则是请求级访问控制，不能代替主机防火墙，也不能保护家庭公网带宽免受 DDoS。

Cloudflare 应设为 DNS-only（灰云）。这种模式不会提供 Cloudflare WAF/DDoS 代理保护；若源站有 IPv6 DNS 记录，源 IPv6 也会公开。应同时配置主机防火墙，只开放 Caddy 的 HTTPS 端口。
