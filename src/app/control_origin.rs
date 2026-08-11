/// 命令可以来自本机人工终端，也可以来自仅供 AI 使用的 `ctl` IPC。
/// 显式区分来源，避免人工恢复操作意外继承 AI 能力检查。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandOrigin {
    HumanTerminal,
    AiCtl,
}
