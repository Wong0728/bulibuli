# B站直播间 API 完整参考

> 基于实测验证，覆盖直播间信息、视频流获取、弹幕/消息系统。
> 所有接口均**无需登录**即可使用（部分功能有限制，文中标注）。

---

## 目录

1. [核心概念](#1-核心概念)
2. [直播间信息接口](#2-直播间信息接口)
3. [直播视频流接口](#3-直播视频流接口)
4. [弹幕/消息系统](#4-弹幕消息系统)
5. [WebSocket 弹幕协议详解](#5-websocket-弹幕协议详解)
6. [完整方案：录制视频+抓取弹幕](#6-完整方案录制视频抓取弹幕)
7. [实测验证记录](#7-实测验证记录)

---

## 1. 核心概念

### 1.1 直播间号体系

- **短号（short_id）**：用户看到的简短号码，如 `1`、`76`
- **长号/真实号（room_id）**：API 实际使用的号码，如 `5440`、`14073662`
- 一个直播间**只有一个长号**，但可能有短号（也可能没有，short_id=0）
- **几乎所有流相关 API 都需要用长号**，拿到短号后第一步是换成长号

### 1.2 直播状态码

| live_status | 含义 |
|---|---|
| 0 | 未开播 |
| 1 | 正在直播 |
| 2 | 轮播中（录播循环播放） |

### 1.3 清晰度代码（qn）

| qn 代码 | 含义 |
|---|---|
| 80 | 流畅 |
| 150 | 高清 |
| 250 | 超清 |
| 400 | 蓝光 |
| 10000 | 原画（1080P） |
| 15000 | 2K |
| 20000 | 4K |
| 30000 | 杜比 |

并非所有直播间都支持全部清晰度，取决于主播推流设置。

### 1.4 域名说明

- API 域名：`api.live.bilibili.com`
- CDN 域名（流地址）：`cn-xxx.bilivideo.com`、`d1--cn-gotcha04.bilivideo.com` 等
- 弹幕 WebSocket：`xxx.chat.bilibili.com`

---

## 2. 直播间信息接口

### 2.1 短号换长号（room_init）

> 必须首先调用的接口，把用户输入的直播间号（可能是短号）转换为真实 room_id。

```
GET https://api.live.bilibili.com/room/v1/Room/room_init?id={直播间号}
```

**参数：**

| 参数 | 类型 | 说明 |
|---|---|---|
| id | num | 直播间号（可以是短号） |

**返回：**

```json
{
  "code": 0,
  "data": {
    "room_id": 14073662,       // 真实长号（后续API都用这个）
    "short_id": 76,            // 短号，0表示无短号
    "uid": 50333369,           // 主播用户mid
    "live_status": 1,          // 0未开播 1直播中 2轮播
    "encrypted": false,        // 是否加密直播间
    "pwd_verified": false,     // 加密房间是否已验证密码
    "is_sp": 0,                // 0普通 1付费直播间
    "special_type": 0,         // 0普通 1付费 2拜年祭
    "live_time": 1602151186    // 开播时间戳（秒），未开播时为 -62170012800
  }
}
```

**错误码：**
- `60004`：直播间不存在

### 2.2 获取直播间详细信息

```
GET https://api.live.bilibili.com/room/v1/Room/get_info?room_id={room_id}
```

**参数：**

| 参数 | 类型 | 说明 |
|---|---|---|
| room_id | num | 直播间号（可以是短号） |

**返回（主要字段）：**

```json
{
  "code": 0,
  "data": {
    "uid": 9617619,              // 主播mid
    "room_id": 5440,             // 真实长号
    "short_id": 1,               // 短号
    "attention": 11919499,       // 关注数
    "online": 81823,             // 观看人数
    "live_status": 1,            // 直播状态
    "title": "直播间标题",        // 标题
    "description": "描述",       // 描述
    "user_cover": "https://...", // 封面图URL
    "keyframe": "https://...",   // 关键帧URL（网页端悬浮展示用）
    "area_id": 377,              // 分区ID
    "area_name": "教育学习",      // 分区名称
    "parent_area_id": 11,        // 父分区ID
    "parent_area_name": "知识",   // 父分区名称
    "live_time": "2026-08-07 20:09:24",  // 开播时间
    "tags": "聊天，情感",         // 标签（逗号分隔）
    "is_portrait": false,        // 是否竖屏
    "room_silent_type": "",      // 禁言状态
    "room_silent_level": 0       // 禁言等级
  }
}
```

### 2.3 通过用户mid查直播间状态

```
GET https://api.live.bilibili.com/room/v1/Room/getRoomInfoOld?mid={用户mid}
```

**返回：**

```json
{
  "data": {
    "roomStatus": 1,          // 0无房间 1有房间
    "roundStatus": 0,         // 0未轮播 1轮播
    "live_status": 1,         // 0未开播 1直播中
    "url": "https://live.bilibili.com/5441",
    "title": "标题",
    "cover": "https://...",
    "online": 268602,
    "roomid": 5441            // 短号
  }
}
```

### 2.4 批量查询直播间状态（无需登录）

```
GET https://api.live.bilibili.com/room/v1/Room/get_status_info_by_uids?uids[]=672328094&uids[]=12345
POST https://api.live.bilibili.com/room/v1/Room/get_status_info_by_uids
Content-Type: application/json
{"uids": [672328094, 12345]}
```

**返回：** 以 uid 为键的对象，每个值包含 `title`、`room_id`、`live_status`、`online`、`uname`、`face`、`cover_from_user`、`keyframe` 等。

### 2.5 获取直播间基本信息（新版，支持批量）

```
GET https://api.live.bilibili.com/xlive/web-room/v1/index/getRoomBaseInfo
    ?req_biz=web_room_componet
    &room_ids=1
    &room_ids=3
```

**返回：** `data.by_room_ids` 以长号为键，包含 `room_id`、`uid`、`live_status`、`title`、`uname`、`cover`、`online`、`attention`、`live_time`、`description`、`tags` 等。

### 2.6 获取主播信息

```
GET https://api.live.bilibili.com/live_user/v1/UserInfo/get_anchor_in_room?roomid={roomid}
```

**返回：** `data.info`（uid、uname、face、认证信息）、`data.level`（主播等级、用户等级）、`data.san`（san值，12满分）

---

## 3. 直播视频流接口

### 3.1 旧版 playUrl（简单直接）

```
GET https://api.live.bilibili.com/room/v1/Room/playUrl
    ?cid={真实room_id}
    &qn=10000
    &platform=web
```

**参数：**

| 参数 | 类型 | 必要 | 说明 |
|---|---|---|---|
| cid | num | 是 | 真实直播间号（长号，不是短号） |
| qn | str | 否 | 清晰度代码，默认150（高清） |
| quality | num | 否 | 与qn二选一：2流畅 3高清 4原画 |
| platform | str | 否 | `h5`=HLS(m3u8)，`web`=http-flv（默认） |

**返回：**

```json
{
  "code": 0,
  "data": {
    "current_quality": 4,
    "accept_quality": ["4", "3", "2"],
    "quality_description": [
      {"qn": 10000, "desc": "原画"},
      {"qn": 400, "desc": "蓝光"},
      {"qn": 250, "desc": "超清"}
    ],
    "durl": [
      {
        "url": "https://d1--cn-gotcha04.bilivideo.com/live-bvc/xxx/live_xxx.flv?expires=xxx&sign=xxx&...",
        "length": 0,
        "order": 1,         // 线路序号，1=主线
        "stream_type": 0,
        "p2p_type": 0
      },
      {
        "url": "https://...",  // 备线2
        "order": 2
      }
    ]
  }
}
```

**关键说明：**
- `durl` 数组包含多条线路（多 CDN），`order=1` 是主线，其他是备线
- URL 带签名参数（`expires`、`sign`、`trid`），**有时效性**
- 返回的 URL 中可能有转义字符（`\u0026` 等），需要处理

### 3.2 新版 getRoomPlayInfo（推荐）

```
GET https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo
    ?room_id={room_id}
    &protocol=0,1          # 0=http_stream(flv), 1=http_hls
    &format=0,1,2          # 0=flv, 1=ts, 2=fmp4
    &codec=0,1             # 0=AVC(H.264), 1=HEVC(H.265)
    &qn=10000
    &platform=web
    &ptype=8
    &dolby=5
    &panorama=1
```

**参数：**

| 参数 | 类型 | 必要 | 说明 |
|---|---|---|---|
| room_id | num | 是 | 直播间id |
| protocol | str | 是 | `0`=http_stream, `1`=http_hls，逗号分隔可多选 |
| format | str | 是 | `0`=flv, `1`=ts, `2`=fmp4，逗号分隔可多选 |
| codec | str | 是 | `0`=AVC(H.264), `1`=HEVC(H.265)，逗号分隔可多选 |
| qn | num | 否 | 清晰度代码，默认150 |
| only_audio | num | 否 | `1`=只返回音频流 |

**返回结构（嵌套树形）：**

```json
{
  "data": {
    "room_id": 23058,
    "live_status": 1,
    "playurl_info": {
      "playurl": {
        "cid": 23058,
        "g_qn_desc": [           // 可用清晰度列表
          {"qn": 10000, "desc": "原画"},
          {"qn": 150, "desc": "高清"}
        ],
        "stream": [              // 协议层
          {
            "protocol_name": "http_stream",
            "format": [          // 格式层
              {
                "format_name": "flv",
                "codec": [       // 编码层
                  {
                    "codec_name": "avc",
                    "current_qn": 10000,
                    "accept_qn": [10000, 150],
                    "base_url": "/live-bvc/462997/live_xxx.flv?",
                    "url_info": [  // 域名层
                      {
                        "host": "https://cn-hbcd-cu-02-20.bilivideo.com",
                        "extra": "expires=xxx&qn=10000&trid=xxx&...",
                        "stream_ttl": 3600
                      },
                      {
                        "host": "https://c1--cn-gotcha208.bilivideo.com",  // 备用CDN
                        "extra": "..."
                      }
                    ]
                  }
                ]
              }
            ]
          },
          {
            "protocol_name": "http_hls",
            "format": [
              {
                "format_name": "ts",
                "codec": [...]
              },
              {
                "format_name": "fmp4",
                "codec": [...]
              }
            ]
          }
        ]
      }
    }
  }
}
```

**最终播放地址拼接：**
```
host + base_url + extra
例：https://cn-hbcd-cu-02-20.bilivideo.com/live-bvc/462997/live_xxx.flv?expires=xxx&qn=10000&...
```

### 3.3 流地址使用注意事项

1. **Referer 必须带**：CDN 会校验 Referer，必须带 `https://live.bilibili.com/`
2. **User-Agent**：使用正常浏览器 UA 即可
3. **URL 时效性**：`expires` 参数控制过期时间（Unix 时间戳），过期后需重新请求 API
4. **ffmpeg 录制命令**：
   ```bash
   ffmpeg -y \
     -user_agent "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36" \
     -referer "https://live.bilibili.com/" \
     -i "<stream_url>" \
     -c copy \
     -movflags +faststart \
     output.mp4
   ```
   - `-c copy`：不重编码，直接复制音视频流
   - 长时间录制需要定期刷新 URL（可通过 WebSocket 的 `PLAYURL_RELOAD` 命令触发）
   - 按 `q` 或发送 `SIGINT` 停止录制，ffmpeg 会自动封装 MP4

### 3.4 完整流程：从直播间号到可播放流

```
用户输入直播间号（可能是短号）
    │
    ▼
① room_init?id=xxx → 拿到真实 room_id
    │
    ▼
② getRoomPlayInfo（新版）或 playUrl（旧版）
    │
    ▼
③ 拼接/选择流 URL → ffmpeg 录制 → MP4
```

---

## 4. 弹幕/消息系统

B站直播间的消息系统通过 **WebSocket 长连接** 实时推送，包含弹幕、礼物、进场、系统通知等多种消息类型。

### 4.1 REST API：获取最近弹幕（最简单）

```
GET https://api.live.bilibili.com/xlive/web-room/v1/dM/gethistory?roomid={room_id}
```

**无需登录，无需签名。** 返回最近 10 条弹幕。

**返回：**

```json
{
  "code": 0,
  "data": {
    "room": [
      {
        "text": "弹幕内容",
        "dm_type": 0,
        "uid": 20276964,
        "nickname": "用户名",
        "timeline": "2026-08-07 22:16:22",
        "isadmin": 0,
        "vip": 0,
        "svip": 0,
        "medal": [27, "粉丝牌名", "主播名", ...],
        "user_level": [59, 0, 16752445, 931],
        "guard_level": 0,
        "wealth_level": 37,
        "id_str": "弹幕ID"
      }
    ],
    "admin": []  // 管理员弹幕，格式同上
  }
}
```

**局限性：** 只能拿最近 10 条，不能实时。要实时弹幕需要 WebSocket。

### 4.2 REST API：获取弹幕配置（颜色/模式）

```
GET https://api.live.bilibili.com/xlive/web-room/v1/dM/GetDMConfigByGroup?room_id={room_id}
```

未登录时只有白色弹幕和滚动模式可用。

---

## 5. WebSocket 弹幕协议详解

### 5.1 连接流程

```
① 获取连接信息（token + 服务器列表）
② 建立 WebSocket 连接
③ 5秒内发送认证包
④ 收到认证回复
⑤ 循环：接收消息 + 每30秒发送心跳包
⑥ 60秒无心跳会被断开
```

### 5.2 获取连接信息

**旧版接口（无需签名，推荐）：**
```
GET https://api.live.bilibili.com/room/v1/Danmu/getConf?room_id={room_id}
```

返回：
```json
{
  "data": {
    "token": "xxx",
    "host_server_list": [
      {
        "host": "broadcastlv.chat.bilibili.com",
        "port": 2243,
        "wss_port": 443,
        "ws_port": 2244
      },
      {
        "host": "bd-gz-live-comet-05.chat.bilibili.com",
        "port": 2243,
        "wss_port": 443,
        "ws_port": 2244
      }
    ]
  }
}
```

**新版接口（需要 Wbi 签名，2025年5月起强制）：**
```
GET https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id={room_id}&type=0&w_rid=xxx&wts=xxx
```

返回 `data.token` + `data.host_list`（结构略有不同，host 字段不含端口，端口单独字段）。

**WebSocket 地址格式：**
```
wss://{host}:{wss_port}/sub
```

### 5.3 数据包格式

所有数据包都是 **固定头部 + 正文** 的二进制格式。

#### 头部结构（16字节）

```
偏移量  长度  类型     含义
0       4    uint32   封包总大小（头部+正文）
4       2    uint16   头部大小（固定 0x0010 = 16字节）
6       2    uint16   协议版本（见下表）
8       4    uint32   操作码（封包类型）
12      4    uint32   sequence（递增序号）
```

#### 协议版本（proto）

| proto | 含义 |
|---|---|
| 0 | 普通包，正文不压缩 |
| 1 | 心跳及认证包，正文不压缩 |
| 2 | 普通包，正文使用 **zlib** 压缩 |
| 3 | 普通包，正文使用 **brotli** 压缩（压缩后的数据可能包含多个子包） |

#### 操作码（operation）

| op | 含义 | 方向 |
|---|---|---|
| 2 | 心跳包 | 上行 |
| 3 | 心跳包回复（人气值） | 下行 |
| 5 | 普通包（命令） | 下行 |
| 7 | 认证包 | 上行 |
| 8 | 认证包回复 | 下行 |

#### Python 打包/解包示例

```python
import struct, json, zlib, brotli

# 打包发送
def make_packet(data: dict, operation: int) -> bytes:
    body = json.dumps(data).encode("utf-8")
    header = struct.pack(">IHHII", 16 + len(body), 16, 1, operation, 1)
    return header + body

# 解包接收
def parse_packet(message: bytes):
    pkt_len, header_len, proto, op, seq = struct.unpack(">IHHII", message[:16])
    body = message[16:pkt_len]
    
    if op == 3:   # 心跳回复，body前4字节是人气值
        popularity = struct.unpack(">I", body[:4])[0]
        return {"type": "heartbeat", "popularity": popularity}
    if op == 8:   # 认证回复
        return {"type": "auth_reply", "data": json.loads(body)}
    if op == 5:   # 命令包
        if proto == 2:
            body = zlib.decompress(body)
        elif proto == 3:
            body = brotli.decompress(body)
        # body 可能包含多个子命令，需要逐个解析
        return {"type": "commands", "raw": body}
```

### 5.4 认证包（Auth）

连接建立后 **5秒内** 必须发送，否则被强制断开。

```json
{
  "uid": 0,           // 0 = 游客（未登录）
  "roomid": 32352630, // 直播间真实 room_id
  "protover": 3,      // 协议版本，3 = brotli 压缩
  "platform": "web",
  "type": 2,
  "key": ""           // token，可为空（游客模式）
}
```

操作码：`7`

认证成功后收到操作码 `8` 的回复：`{"code": 0}`

**注意：** 如果 `uid` 填了具体用户 mid，则 `key` 必须是从 getDanmuInfo 获取的有效 token，否则会被断开。`uid=0` 时 key 可以为空。

### 5.5 心跳包

每 **30秒** 发送一次，正文可为空或任意字符。

```python
ws.send(make_packet({}, 2))  # 操作码 2
```

回复（操作码 3）前 4 字节是 **人气值**（uint32 大端）。

### 5.6 普通包（命令）解析

操作码 5 的包经过解压后，可能包含**多条子命令**。每个子命令也有自己的头部：

```python
def parse_commands(body: bytes):
    """解析子命令列表"""
    offset = 0
    commands = []
    while offset + 16 <= len(body):
        sub_len = struct.unpack(">I", body[offset:offset+4])[0]
        if sub_len < 16 or offset + sub_len > len(body):
            break
        sub_body = body[offset+16:offset+sub_len]
        cmd_data = json.loads(sub_body)
        commands.append(cmd_data)
        offset += sub_len
    return commands
```

### 5.7 命令类型（cmd 字段）

#### 弹幕消息 `DANMU_MSG`

```json
{
  "cmd": "DANMU_MSG",
  "info": [
    [...],           // info[0]: 弹幕元数据数组
    "弹幕文本",       // info[1]: 弹幕内容
    [uid, "用户名", ...],  // info[2]: 发送者信息
    [粉丝牌等级, "粉丝牌名", "主播名", ...],  // info[3]: 粉丝勋章
    [UL等级信息],    // info[4]
    [...],           // info[5]
    0, 0, null,
    {"ct": "xxx", "ts": 1723979200},  // info[9]: 发送时间
    ...
  ]
}
```

**提取弹幕文本：** `info[1]`
**提取用户名：** `info[0][15]["user"]["base"]["name"]` 或 `info[2][1]`
**提取UID：** `info[0][15]["user"]["uid"]` 或 `info[2][0]`
**提取粉丝牌：** `info[3][1]`（牌名）、`info[3][0]`（等级）

**注意：** 未登录用户看到的用户名会被脱敏（如 `倪***`），部分字段会变为 0 或 `*`。

#### 送礼 `SEND_GIFT`

```json
{
  "cmd": "SEND_GIFT",
  "data": {
    "uid": 510149209,
    "uname": "用户名",
    "giftName": "小花花",
    "giftId": 31036,
    "num": 1,
    "price": 100,
    "total_coin": 100,
    "coin_type": "gold",
    "action": "投喂",
    "timestamp": 1673622464,
    "receive_user_info": {
      "uid": 36047134,
      "uname": "主播名"
    }
  }
}
```

#### 进场/关注/分享 `INTERACT_WORD`

```json
{
  "cmd": "INTERACT_WORD",
  "data": {
    "msg_type": 1,     // 1进场 2关注 3分享
    "uid": 335979315,
    "uname": "用户名",
    "timestamp": 1644563948,
    "roomid": 24143902,
    "fans_medal": {
      "medal_level": 1,
      "medal_name": "小豆皮",
      "target_id": 6574487
    }
  }
}
```

已逐步被 `INTERACT_WORD_V2` 替代（V2 使用 protobuf 编码，需额外解析）。

#### 上舰 `GUARD_BUY`

```json
{
  "cmd": "GUARD_BUY",
  "data": {
    "uid": 14225357,
    "username": "用户名",
    "guard_level": 3,    // 1总督 2提督 3舰长
    "num": 1,
    "price": 198000,     // 金瓜子价格（CNY*1000）
    "gift_id": 10003,
    "gift_name": "舰长"
  }
}
```

#### 醒目留言（SC）`SUPER_CHAT_MESSAGE`

```json
{
  "cmd": "SUPER_CHAT_MESSAGE",
  "data": {
    "id": 6522809,
    "uid": 294094150,
    "price": 30,          // CNY
    "message": "SC内容",
    "time": 60,           // 持续秒数
    "user_info": {
      "uname": "用户名",
      "face": "https://头像URL"
    },
    "medal_info": {
      "medal_level": 21,
      "medal_name": "粉丝牌名"
    }
  }
}
```

#### 看过人数 `WATCHED_CHANGE`

```json
{
  "cmd": "WATCHED_CHANGE",
  "data": {
    "num": 6624,
    "text_small": "6624",
    "text_large": "6624人看过"
  }
}
```

#### 直播开始 `LIVE`

```json
{"cmd": "LIVE", "roomid": 23614753, "live_time": 1651036923}
```

#### 直播结束/主播准备中 `PREPARING`

```json
{"cmd": "PREPARING", "roomid": "1017", "round": 0}
```

`round=1` 表示轮播中，`round=0` 表示未轮播。

#### 播放链接刷新 `PLAYURL_RELOAD`

当流地址需要刷新时会推送此命令，可用于长时间录制时更新 ffmpeg 的输入 URL。

#### 互动信息合并 `DM_INTERACTION`

连续多条相同弹幕/送礼/关注时的聚合通知。

#### 礼物连击 `COMBO_SEND`

同一用户连续送同种礼物的连击通知。

#### 通知消息 `NOTICE_MSG`

全站通知（如人气榜排名、特殊礼物等）。

#### 房间信息更新 `ROOM_REAL_TIME_MESSAGE_UPDATE`

```json
{
  "cmd": "ROOM_REAL_TIME_MESSAGE_UPDATE",
  "data": {"roomid": 8618057, "fans": 136, "fans_club": 8}
}
```

### 5.8 未登录限制

- 用户名会被脱敏显示（如 `倪***`）
- 部分用户 mid 变为 0
- 部分房间豁免此限制
- 2025年6月起，新版 getDanmuInfo 接口强制要求 buvid3 cookie

---

## 6. 完整方案：录制视频+抓取弹幕

### 6.1 架构概览

```
输入: 直播间号
    │
    ├── 线程1: 视频录制
    │   ① room_init → room_id
    │   ② playUrl/getRoomPlayInfo → 流 URL
    │   ③ ffmpeg 录制 → MP4
    │   ④ 定期刷新 URL（监听 PLAYURL_RELOAD 或定时重新请求）
    │
    └── 线程2: 弹幕抓取
        ① getConf → token + host
        ② WebSocket 连接 → 认证包
        ③ 心跳保活（30秒间隔）
        ④ 解析命令包 → 存储弹幕/礼物/进场
        ⑤ 关闭时输出 JSON/CSV
```

### 6.2 Python 实现要点

```python
import json, struct, time, zlib, threading
import brotli, websocket, urllib.request

ROOM_ID = 32352630

# === 视频流获取 ===
def get_stream_url(room_id, qn=150):
    url = f"https://api.live.bilibili.com/room/v1/Room/playUrl?cid={room_id}&qn={qn}&platform=web"
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    data = json.loads(urllib.request.urlopen(req, timeout=10).read())
    return data["data"]["durl"][0]["url"]  # 主线 URL

# === WebSocket 弹幕 ===
def get_danmu_hosts(room_id):
    url = f"https://api.live.bilibili.com/room/v1/Danmu/getConf?room_id={room_id}"
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    data = json.loads(urllib.request.urlopen(req, timeout=10).read())
    token = data["data"].get("token", "")
    hosts = data["data"]["host_server_list"]
    return token, hosts

def make_packet(data, operation):
    body = json.dumps(data).encode("utf-8")
    return struct.pack(">IHHII", 16+len(body), 16, 1, operation, 1) + body

def parse_commands(body):
    """从解压后的 body 中提取所有子命令"""
    cmds = []
    offset = 0
    while offset + 16 <= len(body):
        sub_len = struct.unpack(">I", body[offset:offset+4])[0]
        if sub_len < 16 or offset + sub_len > len(body):
            break
        try:
            cmds.append(json.loads(body[offset+16:offset+sub_len]))
        except:
            pass
        offset += sub_len
    return cmds

def handle_command(cmd_data):
    cmd = cmd_data.get("cmd", "")
    if cmd == "DANMU_MSG":
        info = cmd_data.get("info", [])
        text = info[1] if len(info) > 1 else ""
        uname = "?"
        try: uname = info[0][15]["user"]["base"]["name"]
        except:
            try: uname = info[2][1]
            except: pass
        print(f"[弹幕] {uname}: {text}")
    elif cmd == "SEND_GIFT":
        d = cmd_data.get("data", {})
        print(f"[礼物] {d.get('uname','')} -> {d.get('giftName','')} x{d.get('num',0)}")
    elif cmd == "INTERACT_WORD":
        d = cmd_data.get("data", {})
        if d.get("msg_type") == 1:
            print(f"[进场] {d.get('uname','')}")
    elif cmd == "WATCHED_CHANGE":
        print(f"[看过] {cmd_data.get('data',{}).get('text_large','')}")
```

### 6.3 ffmpeg 命令参考

```bash
# 基本录制（copy模式，不重编码）
ffmpeg -y \
  -user_agent "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36" \
  -referer "https://live.bilibili.com/" \
  -i "STREAM_URL" \
  -c copy -movflags +faststart \
  output.mp4

# 限时录制（-t 秒数）
ffmpeg -y \
  -user_agent "Mozilla/5.0" \
  -referer "https://live.bilibili.com/" \
  -i "STREAM_URL" \
  -t 60 -c copy \
  output.mp4

# 录制音频流
ffmpeg -y \
  -user_agent "Mozilla/5.0" \
  -referer "https://live.bilibili.com/" \
  -i "STREAM_URL" \
  -vn -c:a copy \
  output.aac

# 使用 HLS 流（m3u8）
ffmpeg -y \
  -user_agent "Mozilla/5.0" \
  -referer "https://live.bilibili.com/" \
  -i "https://xxx.m3u8?..." \
  -c copy \
  output.mp4
```

---

## 7. 实测验证记录

测试时间：2026-08-07 22:16 (GMT+8)
测试直播间：`https://live.bilibili.com/32352630/`（标题"竹知道了"，8万+在线）

### 7.1 短号换长号

```bash
curl -s 'https://api.live.bilibili.com/room/v1/Room/room_init?id=32352630'
```
结果：`room_id=32352630`（本身即长号），`uid=1612081513`，`live_status=1`

### 7.2 获取流地址（无需登录）

```bash
curl -s 'https://api.live.bilibili.com/room/v1/Room/playUrl?cid=32352630&qn=150&platform=web'
```
结果：成功返回 2 条 FLV 线路，清晰度支持原画/蓝光/超清

### 7.3 ffmpeg 录制

```bash
ffmpeg -y \
  -user_agent "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36" \
  -referer "https://live.bilibili.com/" \
  -i "$STREAM_URL" \
  -t 8 -c copy -movflags +faststart \
  live_sample.mp4
```
结果：成功录制 8 秒，1.1MB，1280x720 H.264 + AAC 48kHz

**不带 Referer 时返回 403 Forbidden。**

### 7.4 获取最近弹幕（REST）

```bash
curl -s 'https://api.live.bilibili.com/xlive/web-room/v1/dM/gethistory?roomid=32352630'
```
结果：成功返回 10 条最近弹幕

### 7.5 WebSocket 弹幕流

```bash
# 获取连接信息
curl -s 'https://api.live.bilibili.com/room/v1/Danmu/getConf?room_id=32352630'
```
结果：成功获取 token + 3 个 WebSocket 服务器

WebSocket 连接测试结果（20秒内捕获）：
```
Auth OK!
[弹幕] 倪***: 不断提高下限
[看过] 6624人看过
[弹幕] 温***: 恐怕也只有我们做了
[弹幕] 今***: 应润则润，不想呆就滚
[弹幕] 咸***: 但是现在做不到，很多是十三休一，早八晚九，月入五千
[弹幕] 今***: 甜甜圈这种炒币狗也配？
[弹幕] 毛***: 晚上好
```

用户名脱敏（`倪***`）是因为 uid=0 游客登录。

### 7.6 验证总结

| 功能 | 是否需要登录 | 是否需要签名 | 状态 |
|---|---|---|---|
| 直播间信息 | ❌ | ❌ | ✅ |
| 流地址获取（旧版） | ❌ | ❌ | ✅ |
| 流地址获取（新版） | ❌ | ❌ | ✅ |
| ffmpeg 录制 | ❌ | - | ✅（需带 Referer） |
| 最近弹幕（REST） | ❌ | ❌ | ✅ |
| 实时弹幕（WebSocket） | ❌（uid=0） | ❌ | ✅（用户名脱敏） |
| getDanmuInfo（新版） | 需 buvid3 | 需 Wbi | ❌ 未测试 |
| 发送弹幕 | ✅ 需登录 | 需 csrf | 未测试 |

---

## 附录：错误码速查

| code | 含义 |
|---|---|
| 0 | 成功 |
| -400 | 参数错误 |
| -352 | 需要 Wbi 签名 |
| -101 | 账号未登录 |
| -111 | csrf 校验失败 |
| 1 | 不存在/错误 |
| 60004 | 直播间不存在 |
| 19002003 | 房间信息不存在 |
| 1002002 | 参数错误 |
