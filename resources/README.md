# 预编译运行时二进制（Windows）

本目录内的可执行文件是 Windows 便携版打包所需的第三方运行时依赖，直接随仓库分发。
`geo/` 子目录下的 GeoIP 数据库为跨平台数据文件，所有平台共用。

Linux / Termux 通过系统包管理器安装对应依赖，不使用本目录文件（见 `deploy/` 安装脚本）。
但 `geo/GeoLite2-Country.mmdb` 在所有平台下都会被程序自动发现并使用。

## 清单

| 文件 | 版本 | 用途 | 官方来源 |
| --- | --- | --- | --- |
| `aria2c.exe` | aria2 1.37.0 | 多线程下载引擎（RPC 模式） | https://github.com/aria2/aria2/releases |
| `ffmpeg.exe` | ffmpeg 5.1.6 | 音视频合并 / 字幕烧录 | https://www.gyan.dev/ffmpeg/builds/ 或 https://ffmpeg.org/download.html |
| `geo/GeoLite2-Country.mmdb` | dbip-country-lite 2026-07 | GeoIP 国家库（用于 `geo cn on` 大陆 IP 配对限制） | https://db-ip.com/db/download/ip-to-country-lite （CC BY 4.0） |

## 校验和（SHA-256）

更新二进制后，请同步更新下表，并在提交信息中说明来源与版本变更：

```
aria2c.exe                   E9B871710234F9DF7C545C8AF2BBDF04D2958B5110CB3D129495CEBF3E649D5B
ffmpeg.exe                   AD90D0C517B4910A7CF521B5366E2CD8D919D26F444A381022D18ABB3F7BA999
geo/GeoLite2-Country.mmdb    B98568CEF7CEE1A588C9C78A9DB02936C4A4D94F20AFF7629DA22C74563B0F5B
```

### 在 Windows 上核对

```powershell
Get-FileHash -Algorithm SHA256 resources\aria2c.exe, resources\ffmpeg.exe, resources\geo\GeoLite2-Country.mmdb |
    Format-List Algorithm, Hash, Path
```

### 在 Linux / macOS 上核对

```bash
sha256sum resources/aria2c.exe resources/ffmpeg.exe resources/geo/GeoLite2-Country.mmdb
```

## 更新与安全说明

- ffmpeg 属高频出 CVE 组件，建议定期跟进官方发布版本；每次更新务必从上述官方来源下载，
  核对上游发布页公布的校验和后再替换，并更新本文件的版本与 SHA-256。
- 二进制无法通过代码审查（diff）核实，校验和是唯一的完整性凭据。请勿从非官方镜像获取。
- `geo/GeoLite2-Country.mmdb` 来源于 DB-IP 官方免费版（IP to Country Lite，CC BY 4.0），
  每月更新。如需更准确或更新的数据，可从 https://db-ip.com/db/download/ip-to-country-lite
  下载最新 `dbip-country-lite-YYYY-MM.mmdb.gz`，解压后替换本文件并同步更新上面的 SHA-256。
  依据 CC BY 4.0，使用该数据库需在面向最终用户的界面中保留 DB-IP 来源说明。
- 当前内置版本仅含 IPv4 数据（database_type=`country ipv4`）。Local 模式下默认监听
  `127.0.0.1`，配对请求均走 IPv4，不受影响；LAN/proxy 模式下若有 IPv6 客户端配对，
  会被安全策略以"无法判断网络区域"拒绝。需要 IPv6 支持时请用 `geo db <path>` 显式
  指定同时包含 IPv4+IPv6 的 mmdb 数据库。
- 如需减小仓库体积或历史膨胀，可改为构建期按固定版本 + SHA-256 校验下载（推荐用 Git LFS 管理本目录）。
