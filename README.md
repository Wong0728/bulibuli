# B站视频监控助手 (Bilibili Video Monitor Assistant)

一个专为 B 站视频“补档”设计的监控与下载工具，旨在帮助用户及时备份视频。支持自动监控 UP 主动态并第一时间完成下载，同时也支持根据博主 UID 查询其视频列表并下载。作者的代码水平比较烂，建议用来个人备份，不适合商用（貌似也商用不了啊……）。

## 主要功能

- **双模式补档**：支持**手动实时查询**与 **自动监控**，第一时间下载备份视频。
- **全平台监看**：支持**局域网内远程访问**，您可以在手机、平板或其他设备上实时监控状况。
- **灵活下载引擎**：支持**本地浏览器下载**或**远程投送至 Aria2/Motrix**，适配多种下载场景。
- **极致画质处理**：支持 **8K/杜比视界/HDR** 解析，内置 `ffmpeg` 自动完成 DASH 音视频无损合并。
- **便捷归档管理**：集成 **B 站扫码登录**，支持按博主 UID 自动创建文件夹，实现自动化分类存储。

##  快速开始

### 前置要求

1. **Aria2**：
   - **推荐方案**：安装并启动 [Motrix](https://www.motrix.app/) (美观且强大的下载管理器，内置 Aria2)。
   - **替代方案**：手动运行 `aria2c --enable-rpc --rpc-listen-port=6800`。
2. **FFmpeg**：项目已在 `resources/` 目录下内置了 `ffmpeg.exe`，无需额外安装。

### 运行方式

#### 方式一：直接运行 (推荐 Windows 用户)

1. 下载最新版本的压缩包并解压。
2. 双击运行 `BilibiliUIDBuild.exe`。
3. 程序启动后会自动打开默认浏览器访问控制台。

#### 方式二：源码运行 (开发者)

1. **环境要求**：Python 3.8+
2. **安装依赖**：
   
   ```bash
   pip install -r requirements.txt
   ```
3. **启动服务**：
   
   ```bash
   python start_server.py
   ```

程序启动后会自动打开默认浏览器访问 `http://localhost:5000`。

## 使用指南

1. **配置 Cookies**：
   - 进入“系统设置”选项卡。
   - 点击“B站扫码登录”，使用手机 B 站 App 扫码。登录成功后，Cookies 会自动保存，以便下载高清视频。不建议输入cookies登录，能请求但容易被风控。
2. **手动下载**：
   - 在“手动查询”页面输入 UP 主的 UID（纯数字）。
   - 点击查询，选择视频音频点击下载。
3. **自动补档监控**：
   - 在“自动任务”页面点击“添加博主”，可以单独对每个 UP 主设置轮询时间，系统会在你设置的最大最小秒数中随机选择一个时间间隔进行查询。
4. **下载设置**：
   - 在“系统设置”中可以调整画质偏好、并行下载数、Aria2 连接信息等。

## 开源库与致谢

本项目集成了以下优秀的开源项目，感谢他们的贡献：

### 后端 (Python)

- [Flask](https://flask.palletsprojects.com/): 轻量级 Web 框架。
- [Flask-SocketIO](https://flask-socketio.readthedocs.io/): 实现实时双向通信。
- [SQLAlchemy](https://www.sqlalchemy.org/) & [Flask-SQLAlchemy](https://flask-sqlalchemy.palletsprojects.com/): 数据库 ORM 管理。
- [APScheduler](https://apscheduler.readthedocs.io/): 后台定时任务调度。
- [Requests](https://requests.readthedocs.io/): 处理 HTTP 请求。
- [Eventlet](https://eventlet.net/): 高性能并发支持。

### 前端 (Web)

- [Socket.IO](https://socket.io/): 实时通信客户端。
- [QRCode.js](https://davidshimjs.github.io/qrcodejs/): 生成登录二维码。
- [Font Awesome](https://fontawesome.com/): 图标支持。
- [JetBrains Mono](https://www.jetbrains.com/lp/mono/): 专为开发者设计的字体。

### 工具软件

- [Aria2](https://aria2.github.io/): 高性能下载引擎。
- [Motrix](https://www.motrix.app/): 推荐的 Aria2 图形化管理工具。
- [FFmpeg](https://ffmpeg.org/): 视频流处理与合并工具。

## 免责声明

1. **用途限制**：本项目仅供技术研究、学习和交流使用。请勿将其用于任何商业用途或非法活动。
2. **版权声明**：通过本工具下载的所有视频内容，其版权均归原作者及平台所有。用户在使用过程中应遵守相关法律法规，尊重原作者权益。
3. **账号安全**：本项目涉及的 Cookies 和登录信息均保存在本地，开发者不会收集您的任何个人信息。因用户操作不当导致的账号风险由用户自行承担。
4. **不保证性**：开发者不保证 B 站 API 的永久有效性，且不对因使用本项目导致的任何损失负责。

## 开源协议

本项目采用 **[CC BY-NC 4.0 (署名-非商业性使用)](LICENSE)** 协议开源。

- **您可以**：自由地共享（在任何媒介以任何形式复制、发行本作品）和演绎（修改、转换或以本作品为基础进行创作）。
- **署名**：您必须给出适当的署名，提供一份本许可协议的链接，同时标明是否对本作品作了修改。

- **非商业性使用**：您不得将本作品用于商业目的。
