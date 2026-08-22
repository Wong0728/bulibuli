# 运行时资源

Windows 的 `aria2c.exe` 和 `ffmpeg.exe` 作为已审计资源存放在仓库中。完整 Release 构建会复制对应平台的 `aria2c`、`ffmpeg` 及必要的非系统动态库到发布目录，并为每个文件写入独立 `.sha256` 文件；macOS 若 CI runner 有 `ffprobe` 也会随包提供，Linux 则使用自编译的精简 `ffmpeg`/`ffprobe`（见下文）。Termux 仍使用 `pkg` 安装。Linux/macOS 的 `core` 命令包不包含这些媒体运行时，交给本机环境提供。
`geo/` 子目录下的 GeoIP 数据库为跨平台数据文件，所有平台共用。`geo/GeoLite2-Country.mmdb` 在所有平台下都会被程序自动发现并使用。

## 清单

| 文件 | 版本 | 用途 | 官方来源 |
| --- | --- | --- | --- |
| `aria2c.exe` | aria2 1.37.0 | 多线程下载引擎（RPC 模式） | https://github.com/aria2/aria2/releases |
| `ffmpeg.exe` | ffmpeg n8.1.2（本项目 Actions 自编译精简版） | 直播录制（http/FLV）/ 音视频合并 / remux / 弹幕字幕烧录 | `.github/workflows/build-ffmpeg.yml`（源码 https://github.com/FFmpeg/FFmpeg/tree/n8.1.2 ） |
| `ffprobe.exe` | ffmpeg n8.1.2（与 `ffmpeg.exe` 同次构建） | 媒体流校验与时长探测；缺失时由 FFmpeg 回退 | 同上 |
| `aria2c` / `ffmpeg` | aria2 1.37.0（系统包）/ ffmpeg n8.1.2（本项目 Actions 自编译精简版，glibc 2.35 基线） | Linux/macOS 便携包运行时 | https://github.com/aria2/aria2/releases / `.github/workflows/build-ffmpeg.yml`（linux job） |
| `geo/GeoLite2-Country.mmdb` | dbip-country-lite 2026-07 | GeoIP 国家库（用于 `geo cn on` 大陆 IP 配对限制） | https://db-ip.com/db/download/ip-to-country-lite （CC BY 4.0） |

## 内置 FFmpeg 构建说明

Windows 包内 `ffmpeg.exe` / `ffprobe.exe` 由本仓库的 GitHub Actions 工作流
`.github/workflows/build-ffmpeg.yml` 交叉编译（ubuntu runner + mingw-w64，
全静态链接，TLS 走 Windows 系统 schannel），只保留项目实际使用的能力：

- 直播录制：`file/tcp/tls/http/https` 协议、FLV demuxer/muxer、reconnect/user_agent 选项
- 分段合并：concat demuxer、MP4 muxer（faststart）
- 音视频合并 / remux：mov/matroska/mpegts/mp3/aac/ac3/eac3/flac 容器（全部 `-c copy`）
- 弹幕/字幕烧录：ass 滤镜（libass 0.17.3 + DirectWrite 字体后端）、libx264 编码器、
  h264/hevc/av1/vp9 解码器

外部依赖版本记录在本目录 `BUILD_INFO.txt`（与产物内一致）：x264（master 固定 commit）、
FreeType 2.13.3、FriBidi 1.0.16、HarfBuzz 8.5.0、libass 0.17.3。
整体许可证为 GPL-2.0+（因链接 libx264）。重新构建：Actions 页面手动触发
"Build minimal FFmpeg"，下载 artifact 后替换本目录两个 exe 与 `BUILD_INFO.txt`，
并更新下方校验和。

## 校验和（SHA-256）

更新二进制后，请同步更新下表，并在提交信息中说明来源与版本变更：

```
aria2c.exe                   E9B871710234F9DF7C545C8AF2BBDF04D2958B5110CB3D129495CEBF3E649D5B
ffmpeg.exe                   4502C83A2C4389CF8B076F68F95D78403C9CBABE468BD4C6E7D8F2E583697A32
ffprobe.exe                  BA7B3376653F017A292DC8BC9EA45BA35FF36853ECAE0DB1064F7D5ABC28C141
geo/GeoLite2-Country.mmdb    B98568CEF7CEE1A588C9C78A9DB02936C4A4D94F20AFF7629DA22C74563B0F5B
```

### 在 Windows 上核对

```powershell
Get-FileHash -Algorithm SHA256 resources\aria2c.exe, resources\ffmpeg.exe, resources\ffprobe.exe, resources\geo\GeoLite2-Country.mmdb |
    Format-List Algorithm, Hash, Path
```

### 在 Linux / macOS 上核对

```bash
# 源码仓库只有 Windows 运行时与 GeoIP 数据可核对（Unix 的 aria2c/ffmpeg 仅存在于 Release 归档内）
sha256sum resources/geo/GeoLite2-Country.mmdb
```

源码仓库没有 `resources/aria2c`、`resources/ffmpeg` 和 `resources/ffprobe` 这三个 Unix 文件；前两个只在完整 Release 归档内生成，`ffprobe` 是否随 CI 产物提供取决于 runner，`core` 归档不包含媒体运行时。

## Linux / macOS 运行时说明

Unix 便携包的 `resources/` 布局（Release 归档内，源码仓库不含这些文件）：

- `aria2c`、`ffmpeg`：极小的 shell 包装脚本，负责设置库搜索路径（`LD_LIBRARY_PATH` /
  `DYLD_LIBRARY_PATH`）后启动同目录的真正二进制；`chmod +x` 需要覆盖它们。
- `aria2c.bin`、`ffmpeg.bin`、`ffprobe.bin`：真正的可执行文件。
- `lib/`：随包携带的非系统动态库（构建时由 `ldd`/`otool` 递归收集），
  每个文件旁有同名 `.sha256` 校验文件。
- Linux 的 `ffmpeg.bin`/`ffprobe.bin` 来自 `.github/workflows/build-ffmpeg.yml`
  的 linux job：在 ubuntu:22.04 容器内编译（glibc 2.35 兼容基线），媒体依赖
  （x264/libass 等）静态链接，TLS 用 GnuTLS、字体用 Fontconfig 走系统动态库。
  能力集与 Windows 版一致，体积远小于发行版完整 FFmpeg；
  构建参数记录在归档内 `resources/BUILD_INFO.txt`。
- Linux 的 `aria2c` 来自 CI runner 的系统包管理器（apt），其非系统动态库同样
  被收集进 `lib/`。

## 更新与安全说明

- 内置 FFmpeg 为自编译产物：来源是本仓库 Actions 工作流 + 上游固定版本源码，
  构建日志与产物内 `BUILD_INFO.txt` 即完整性凭据；每次重建后务必更新上方 SHA-256。
- 若改用第三方预编译版（如 gyan.dev / BtbN），必须从官方发布页下载并核对上游校验和，
  同时确认其包含直播录制（http/FLV/concat）与烧录（ass 滤镜、libx264）所需能力。
- ffmpeg 属高频出 CVE 组件，建议定期跟进上游版本；更新时同步调整工作流中的
  `ffmpeg_ref` 并重新构建。
- `geo/GeoLite2-Country.mmdb` 来源于 DB-IP 官方免费版（IP to Country Lite，CC BY 4.0），
  每月更新。如需更准确或更新的数据，可从 https://db-ip.com/db/download/ip-to-country-lite
  下载最新 `dbip-country-lite-YYYY-MM.mmdb.gz`，解压后替换本文件并同步更新上面的 SHA-256。
  依据 CC BY 4.0，使用该数据库需在面向最终用户的界面中保留 DB-IP 来源说明。
- 当前内置版本仅含 IPv4 数据（database_type=`country ipv4`）。Local 模式下默认监听
  `127.0.0.1`，配对请求均走 IPv4，不受影响；LAN/proxy 模式下若有 IPv6 客户端配对，
  会被安全策略以"无法判断网络区域"拒绝。需要 IPv6 支持时请用 `geo db <path>` 显式
  指定同时包含 IPv4+IPv6 的 mmdb 数据库。
- 内置 FFmpeg 已精简为按需能力（约 15 MB/exe，原通用完整构建约 144 MB/exe）；
  如仍需进一步减小仓库体积，可改用 Git LFS 管理本目录或构建期下载。
