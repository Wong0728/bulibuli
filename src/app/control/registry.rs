//! 命令清单：`COMMAND_REGISTRY` 是 help 输出与 `docs/skill.md` 的唯一真实来源。
//!
//! `help` 按分类自动生成分组清单；单测断言 `docs/skill.md` 与此同步，文档漂移时测试失败。

use serde_json::{json, Value};

/// 命令分类。`help` 输出按此分组；后续 `docs/skill.md` 也按此分节。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandCategory {
    Policy,
    Download,
    Blogger,
    System,
    Credential,
}

impl CommandCategory {
    fn key(self) -> &'static str {
        match self {
            CommandCategory::Policy => "policy",
            CommandCategory::Download => "download",
            CommandCategory::Blogger => "blogger",
            CommandCategory::System => "system",
            CommandCategory::Credential => "credential",
        }
    }

    fn title(self) -> &'static str {
        match self {
            CommandCategory::Policy => "策略 - 监听模式、访问规则、AI 开关",
            CommandCategory::Download => "下载 - 入队、暂停、重试、烧录",
            CommandCategory::Blogger => "博主 - 搜索、收藏、自动任务",
            CommandCategory::System => "系统 - 状态、配置、日志、关停",
            CommandCategory::Credential => "凭证 - B 站登录、会话、配对",
        }
    }
}

/// 单条命令的元数据：名称、分类、一句话说明、调用示例。
/// `help` 自动遍历 `COMMAND_REGISTRY` 生成分组清单；P3 的 `docs/skill.md` 同步测试也以此为准。
struct CommandSpec {
    name: &'static str,
    category: CommandCategory,
    desc: &'static str,
    example: &'static str,
}

const COMMAND_REGISTRY: &[CommandSpec] = &[
    // 策略
    CommandSpec {
        name: "mode local|lan|proxy <domain>",
        category: CommandCategory::Policy,
        desc: "切换监听模式（重启后生效）",
        example: "mode lan",
    },
    CommandSpec {
        name: "access default|allow|deny|remove|list",
        category: CommandCategory::Policy,
        desc: "管理 IP 访问规则",
        example: "access allow 192.168.1.0/24 --minutes 60",
    },
    CommandSpec {
        name: "ai on|off",
        category: CommandCategory::Policy,
        desc: "切换 AI Skill 模式（ctl 命令门控）",
        example: "ai on",
    },
    CommandSpec {
        name: "geo cn on|off / geo db <path|remove>",
        category: CommandCategory::Policy,
        desc: "大陆 IP 限制 / GeoIP 数据库",
        example: "geo cn on",
    },
    CommandSpec {
        name: "trust aria2|ffmpeg <value|remove>",
        category: CommandCategory::Policy,
        desc: "信任外部 aria2 / FFmpeg",
        example: "trust ffmpeg /usr/bin/ffmpeg",
    },
    // 下载
    CommandSpec {
        name: "dl status",
        category: CommandCategory::Download,
        desc: "查看队列状态（任务数、健康度）",
        example: "dl status",
    },
    CommandSpec {
        name: "dl add <BV>",
        category: CommandCategory::Download,
        desc: "入队下载（仅支持 BV 号；AV/ep/ss/fp 暂未支持）",
        example: "dl add BV1xx411c7mD",
    },
    CommandSpec {
        name: "dl pause <task_id|all>",
        category: CommandCategory::Download,
        desc: "暂停任务（all 暂停全部）",
        example: "dl pause 123",
    },
    CommandSpec {
        name: "dl resume <task_id|all>",
        category: CommandCategory::Download,
        desc: "恢复任务（all 恢复全部）",
        example: "dl resume all",
    },
    CommandSpec {
        name: "dl retry <bvid> [video|audio] | all-failed",
        category: CommandCategory::Download,
        desc: "重试任务（all-failed 重试全部失败）",
        example: "dl retry BV1xx411c7mD video",
    },
    CommandSpec {
        name: "dl remove <bvid> [video|audio]",
        category: CommandCategory::Download,
        desc: "移除任务（默认 video）",
        example: "dl remove BV1xx411c7mD",
    },
    CommandSpec {
        name: "dl priority <bvid> <level> [video|audio]",
        category: CommandCategory::Download,
        desc: "调整优先级（1..=300，默认 video）",
        example: "dl priority BV1xx411c7mD 200",
    },
    // 博主
    CommandSpec {
        name: "blg search <keyword>",
        category: CommandCategory::Blogger,
        desc: "按名字搜索 UP 主（B 站搜索）",
        example: "blg search 老番茄",
    },
    CommandSpec {
        name: "blg add <uid>",
        category: CommandCategory::Blogger,
        desc: "添加博主为监控任务（自动拉取资料）",
        example: "blg add 12345",
    },
    CommandSpec {
        name: "blg list [monitor|saved]",
        category: CommandCategory::Blogger,
        desc: "列出监控 / 收藏博主（默认全部）",
        example: "blg list monitor",
    },
    CommandSpec {
        name: "blg del <uid>",
        category: CommandCategory::Blogger,
        desc: "删除博主（先试监控，再试收藏）",
        example: "blg del 12345",
    },
    CommandSpec {
        name: "blg monitor on|off <uid>",
        category: CommandCategory::Blogger,
        desc: "启停博主监控",
        example: "blg monitor on 12345",
    },
    // 系统
    CommandSpec {
        name: "sys status",
        category: CommandCategory::System,
        desc: "完整系统状态（运行时长、模式、aria2）",
        example: "sys status",
    },
    CommandSpec {
        name: "sys config",
        category: CommandCategory::System,
        desc: "查看安全配置",
        example: "sys config",
    },
    CommandSpec {
        name: "sys aria2-restart",
        category: CommandCategory::System,
        desc: "重启 Aria2 引擎",
        example: "sys aria2-restart",
    },
    CommandSpec {
        name: "sys ffmpeg-test",
        category: CommandCategory::System,
        desc: "探测并测试 FFmpeg 可用性",
        example: "sys ffmpeg-test",
    },
    CommandSpec {
        name: "sys logs",
        category: CommandCategory::System,
        desc: "查看日志文件路径",
        example: "sys logs",
    },
    CommandSpec {
        name: "sys refresh board|blogger|video <bvid>",
        category: CommandCategory::System,
        desc: "触发刷新（board / blogger / 单视频）",
        example: "sys refresh video BV1xx411c7mD",
    },
    CommandSpec {
        name: "quit",
        category: CommandCategory::System,
        desc: "优雅关停程序",
        example: "quit",
    },
    // 凭证
    CommandSpec {
        name: "cred qrcode",
        category: CommandCategory::Credential,
        desc: "取扫码登录二维码 URL",
        example: "cred qrcode",
    },
    CommandSpec {
        name: "cred qrcode-poll <qrcode_key>",
        category: CommandCategory::Credential,
        desc: "轮询扫码状态（code=0 成功）",
        example: "cred qrcode-poll abc123",
    },
    CommandSpec {
        name: "cred status",
        category: CommandCategory::Credential,
        desc: "查看 B 站登录状态",
        example: "cred status",
    },
    CommandSpec {
        name: "pair [close]",
        category: CommandCategory::Credential,
        desc: "服务器终端开启 / 关闭配对模式",
        example: "pair",
    },
    CommandSpec {
        name: "sessions",
        category: CommandCategory::Credential,
        desc: "列出已配对会话",
        example: "sessions",
    },
    CommandSpec {
        name: "revoke <id|all>",
        category: CommandCategory::Credential,
        desc: "撤销指定会话或全部会话",
        example: "revoke all",
    },
    // 审计与事件（P2）
    CommandSpec {
        name: "audit list [--source <s>] [--since <1h|24h|7d>] [--limit N]",
        category: CommandCategory::System,
        desc: "查询审计日志（按来源/时间过滤）",
        example: "audit list --source ai_skill --since 1h",
    },
    CommandSpec {
        name: "audit by-target <task|blogger|cookie|session> <id>",
        category: CommandCategory::System,
        desc: "按目标资源查操作历史",
        example: "audit by-target task 42",
    },
    CommandSpec {
        name: "events [--watch] [--limit N]",
        category: CommandCategory::System,
        desc: "查看最近事件（--watch 流式订阅）",
        example: "events --watch",
    },
];

pub(super) fn help() -> Value {
    // 按 category 分组，保持 COMMAND_REGISTRY 中的声明顺序
    let mut themes: Vec<Value> = Vec::new();
    for category in [
        CommandCategory::Policy,
        CommandCategory::Download,
        CommandCategory::Blogger,
        CommandCategory::System,
        CommandCategory::Credential,
    ] {
        let commands: Vec<Value> = COMMAND_REGISTRY
            .iter()
            .filter(|spec| spec.category == category)
            .map(|spec| {
                json!({
                    "name": spec.name,
                    "desc": spec.desc,
                    "example": spec.example,
                })
            })
            .collect();
        themes.push(json!({
            "name": category.key(),
            "title": category.title(),
            "commands": commands,
        }));
    }
    json!({
        "themes": themes,
        "expert_mode_hint": "TUI 输入 `> <command>` 直接执行扁平命令；IPC 客户端 `ctl <command>` 即扁平命令",
        "ai_mode_note": "未启用 AI Skill 模式时，ctl 放行 status/help/quit/ai/pair/sys status",
    })
}

/// 从 `COMMAND_REGISTRY` 生成 `docs/skill.md` 的完整内容。
///
/// 这是 AI Skill 文档的唯一真实来源：命令清单、示例、错误码均从此函数生成。
/// 单测 `test_skill_doc_in_sync` 断言 `docs/skill.md` 内容 == 此函数输出，
/// 文档漂移时测试失败，强制保持同步。
#[cfg(test)]
fn generate_skill_markdown() -> String {
    let mut out = String::new();
    out.push_str("# 补哩补哩 Skill\n\n");
    out.push_str(
        "> 本文档供本机 AI 调用 `ctl` 命令使用，由 `COMMAND_REGISTRY` 自动生成，请勿手改。\n",
    );
    out.push_str(
        "> 命令清单与代码同步：修改命令后运行 `cargo test test_skill_doc_in_sync` 更新文档。\n\n",
    );

    // 调用方式
    out.push_str("## 调用方式\n\n");
    out.push_str("所有命令通过本机 IPC 调用（无网络端口暴露）：\n\n");
    out.push_str("```\nbulibuli.exe ctl <command> [args...]\n```\n\n");
    out.push_str("返回 JSON 信封：\n");
    out.push_str("- 成功：`{\"ok\": true, \"data\": {...}}`\n");
    out.push_str("- 失败：`{\"ok\": false, \"error\": \"...\", \"code\": \"...\"}`\n\n");

    // 前置条件
    out.push_str("## 前置条件\n\n");
    out.push_str("1. **AI Skill 模式已启用**：网页 Setup 向导步骤 3 选启用，或运行 `ai on`。");
    out.push_str(
        "未启用时放行 `status` / `help` / `quit` / `ai` / `pair` / `sys status`，其他命令返回 `AI_SKILL_DISABLED`。\n",
    );
    out.push_str(
        "启用后 AI 助手拥有与人工相同的全部操作权限（含 `mode` / `access` / `geo` / `trust` / `pair` 等基础配置命令），无需任何临时授权；所有 ctl 命令都要求服务已在运行。\n",
    );
    out.push_str(
        "2. **B 站已登录**：涉及 B 站 API 的命令（download / blogger / cookies / refresh）",
    );
    out.push_str("需先扫码登录，未登录返回 `BILI_NOT_LOGGED_IN`。\n");
    out.push_str(
        "3. **本机权限**：命名管道仅本机进程可连接（SDDL 限制为系统/管理员/所有者）。\n\n",
    );

    // 命令清单
    out.push_str("## 命令清单\n\n");
    for category in [
        CommandCategory::Policy,
        CommandCategory::Download,
        CommandCategory::Blogger,
        CommandCategory::System,
        CommandCategory::Credential,
    ] {
        out.push_str(&format!("### {}\n\n", category.title()));
        out.push_str("| 命令 | 说明 | 示例 |\n");
        out.push_str("|---|---|---|\n");
        for spec in COMMAND_REGISTRY.iter().filter(|s| s.category == category) {
            out.push_str(&format!(
                "| `{}` | {} | `{}` |\n",
                spec.name, spec.desc, spec.example
            ));
        }
        out.push('\n');
    }

    // 乐观锁
    out.push_str("## 乐观并发控制\n\n");
    out.push_str("状态变更类操作支持乐观锁，调用方传 `--expected-version N`：\n\n");
    out.push_str("```\ndl pause <task_id> --expected-version 42\n```\n\n");
    out.push_str("- 当前 version 匹配时执行 + version += 1，返回 `{\"ok\": true, \"data\": {\"new_version\": 43}}`\n");
    out.push_str("- 不匹配时返回 `CONFLICT` 错误 + 当前状态，调用方可重新读状态后重试\n");
    out.push_str("- 不传 `--expected-version` 时按「最后写入胜出」语义\n\n");

    // 事件流
    out.push_str("## 实时事件订阅\n\n");
    out.push_str("```\nbulibuli.exe ctl events --watch\n```\n\n");
    out.push_str("流式输出 JSON Lines（每行一条审计事件），30 秒无事件发空行保活。\n");
    out.push_str("敏感操作（cookie 保存、pair code 生成）不广播，仅在审计日志中可查。\n\n");

    // 端到端流程
    out.push_str("## 端到端流程示例\n\n");
    out.push_str("### 场景 1：下载某 UP 主最新视频\n\n");
    out.push_str("```\nblg search <name>       # 搜索 UP 主拿 uid\nblg add <uid>           # 添加监控\ndl add <BV1xx411c7mD>   # 入队下载\ndl status               # 轮询队列直到 completed\n```\n\n");
    out.push_str("### 场景 2：扫码登录 B 站\n\n");
    out.push_str("```\ncred qrcode              # 取二维码 URL + qrcode_key\n# 提示用户用 B 站 App 扫码\ncred qrcode-poll <key>   # 每 2 秒轮询，code=0 表示成功\ncred status              # 确认登录状态\n```\n\n");

    // 错误码
    out.push_str("## 错误码\n\n");
    out.push_str("| 错误码 | 含义 |\n");
    out.push_str("|---|---|\n");
    out.push_str("| `AI_SKILL_DISABLED` | AI Skill 模式未启用，先执行 `ai on` |\n");
    out.push_str("| `BILI_NOT_LOGGED_IN` | B 站未登录，先执行 `cred qrcode` 扫码 |\n");
    out.push_str("| `CONFLICT` | 乐观锁冲突，重新读状态后重试 |\n");
    out.push_str("| `BAD_REQUEST` | 参数错误 |\n");
    out.push_str("| `NOT_FOUND` | 资源不存在 |\n");
    out.push_str("| `RISK_CONTROL` | 触发 B 站风控 |\n");
    out.push_str("| `INTERNAL` | 内部错误 |\n");

    out
}

#[cfg(test)]
mod skill_doc_tests {
    use super::generate_skill_markdown;
    use std::path::PathBuf;

    /// 断言 `docs/skill.md` 与 `generate_skill_markdown()` 输出一致。
    /// 修改命令清单后运行 `cargo test test_skill_doc_in_sync` —— 失败时把
    /// `generate_skill_markdown()` 输出覆写回 `docs/skill.md` 即可。
    #[test]
    fn test_skill_doc_in_sync() {
        let expected = generate_skill_markdown();
        let skill_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("skill.md");
        let actual = std::fs::read_to_string(&skill_path)
            .map(|content| content.replace("\r\n", "\n"))
            .unwrap_or_else(|error| {
            panic!(
                "读取 {} 失败: {error}\n请运行测试前先创建该文件（内容 = generate_skill_markdown() 输出）",
                skill_path.display()
            )
        });
        assert_eq!(
            expected,
            actual,
            "docs/skill.md 与 COMMAND_REGISTRY 不同步。\n\
             修改命令后请用 generate_skill_markdown() 的输出更新 docs/skill.md。\n\
             路径: {}",
            skill_path.display()
        );
    }

    /// 生成 `docs/skill.md`：仅在文件缺失时写入，用于首次初始化。
    /// 后续命令变更后请手动删除 docs/skill.md 再跑此测试，或直接用
    /// `generate_skill_markdown()` 输出覆写。
    #[test]
    fn write_skill_doc_if_missing() {
        let skill_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("skill.md");
        if !skill_path.exists() {
            std::fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
            std::fs::write(&skill_path, generate_skill_markdown()).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_groups_all_categories() {
        let value = help();
        let themes = value
            .get("themes")
            .and_then(|v| v.as_array())
            .expect("help 应含 themes 数组");
        assert_eq!(themes.len(), 5, "应有 5 个主题分组");
        for theme in themes {
            let commands = theme
                .get("commands")
                .and_then(|v| v.as_array())
                .expect("每个主题应有 commands 数组");
            assert!(!commands.is_empty(), "每个主题至少有 1 条命令");
        }
    }

    #[test]
    fn command_registry_has_no_duplicate_names() {
        let mut names: Vec<&str> = COMMAND_REGISTRY.iter().map(|s| s.name).collect();
        names.sort_unstable();
        let initial = names.len();
        names.dedup();
        assert_eq!(names.len(), initial, "COMMAND_REGISTRY 中命令名不应重复");
    }

    #[test]
    fn pair_command_has_no_temporary_authorization_requirement() {
        // AI 模式开启后 AI 与人工权限一致（审计 2026-08-18 B2：删除 ai assist 短时授权层），
        // 帮助清单里 pair 不应再出现"临时授权"字样。
        let pair = COMMAND_REGISTRY
            .iter()
            .find(|spec| spec.name.starts_with("pair "))
            .expect("pair 应在 COMMAND_REGISTRY 中");
        assert!(!pair.desc.contains("授权"));
    }
}
