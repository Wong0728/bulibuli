/// Commands can enter through the local human console or through the AI-only
/// `ctl` IPC. Keeping this explicit prevents a human recovery action from
/// accidentally inheriting AI capability checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandOrigin {
    HumanTerminal,
    AiCtl,
}
