# B站视频监控助手 - 开发者文档

本文档旨在为开发者提供项目的架构说明、模块功能及开发指南。(写的太差了，真心不建议基于本项目开发)

##  项目架构

项目采用前后端分离的架构，为了部署方便，前端静态资源由 Flask 后端托管。

```text
Cg_Bilibiliuid/
├── backend/                # 后端代码目录
│   ├── routes/             # API 路由处理 (Blogger, Video, Download等)
│   ├── services/           # 核心业务逻辑 (BiliAPI, Aria2, Monitor等)
│   ├── utils/              # 工具函数 (Cookies处理, 辅助函数)
│   ├── app.py              # Flask 应用工厂
│   ├── models.py           # SQLAlchemy 数据库模型
│   └── requirements.txt    # 后端依赖
├── resources/              # 静态资源 (ffmpeg.exe等)
├── css/                    # 前端样式
├── js/                     # 前端脚本 (app.js)
├── index.html              # 主界面
├── start_server.py         # 启动入口脚本
└── requirements.txt        # 项目依赖
```

## 核心模块说明

### 1. BiliAPI (`backend/services/bili_api.py`)
负责与 B 站 API 通信，包含：
- WBI 签名算法实现。
- 视频信息获取、下载链接解析（DASH/FLV）。
- 扫码登录流程处理。

### 2. Aria2 下载管理器 (`backend/services/aria2_service.py`)
- **外部 RPC 模式**：核心设计为连接外部 Aria2 服务（如 Motrix）。
- **状态同步**：实时轮询 RPC 接口获取进度，并统一格式化后推送到前端。

### 3. 监控服务 (`backend/services/monitor_service.py`)
- 基于 `APScheduler` 实现。
- 每个博主对应一个独立的调度任务。
- 自动对比数据库记录，识别并处理新视频。

### 4. 视频处理器 (`backend/services/video_processor.py`)
- 调用内置的 `ffmpeg`。
- 处理 DASH 格式视频（音视频分离）的合并。

##  数据库设计

项目使用 SQLite 数据库 (`backend/data/app.db`)，主要表结构：
- `bloggers`: 存储监控博主的信息及任务状态。
- `download_history`: 记录已下载视频的信息，防止重复下载。
- `settings`: 存储系统配置项。

##  数据库迁移

如果项目更新涉及数据库结构变动，可以使用 `backend/migrate_db.py` 脚本进行迁移。
- 运行方式：`python backend/migrate_db.py`
- 该脚本目前支持将 `download_tasks` 表的唯一约束从 `bvid` 修改为 `bvid + type` 复合约束。

##  实时通信 (WebSocket)

使用 `Flask-SocketIO` 实现实时通信：
- `download_status`: 推送下载进度。
- `blogger_log`: 推送监控任务日志。
- `aria2_status`: 推送 Aria2 服务连接状态。

##  开发建议

1. **扩展 API**：如需增加新的 B 站接口，请参考[某知名BilibBiliApi项目（补档)](https://framecode.feishu.cn/file/LT94bcPwRo9yKNxVCSxcq3hFnwh?from=from_copylink)
2. **前端修改**：`js/app.js` 是前端核心逻辑，采用了简单的状态管理和 Tab 切换机制。


##  贡献指南

欢迎大佬们提交 Issue ，感谢批评指正！