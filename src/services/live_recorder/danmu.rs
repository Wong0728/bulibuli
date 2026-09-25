//! 弹幕采集装配：启动 collector 与互动归档 writer 任务，并监督 collector 的
//! 生命周期事件（正常退出 / 异常失败），供录制 worker 降级处理。

use super::interactions::{self, InteractionPaths, InteractionWriterArgs};
use super::RecordingInfo;
use crate::services::bili_api::BiliApi;
use crate::services::danmu_collector::DanmuCollector;
use crate::services::live_source::CaptureMode;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// collector 生命周期事件。
#[derive(Debug)]
pub(super) enum DanmuCollectorEvent {
    Exited,
    Failed(String),
}

/// 弹幕采集链路的启动产物：监督任务、writer 任务、事件通道与持久化路径。
pub(super) struct DanmuStartup {
    pub(super) monitor: Option<JoinHandle<()>>,
    pub(super) writer: Option<JoinHandle<anyhow::Result<()>>>,
    pub(super) collector_rx: mpsc::Receiver<DanmuCollectorEvent>,
    pub(super) failure_rx: mpsc::Receiver<String>,
    pub(super) reload_rx: mpsc::Receiver<()>,
    pub(super) persisted_path: Option<PathBuf>,
    /// collector 是否成功启动（决定 worker 是否监听对应事件通道）。
    pub(super) channel_open: bool,
    pub(super) unavailable: bool,
}

/// 启动失败路径的统一清理：取消 collector 并中止监督与 writer 任务。
pub(super) fn abort_danmu_tasks(
    collector_cancel: &CancellationToken,
    monitor: &mut Option<JoinHandle<()>>,
    writer: &mut Option<JoinHandle<anyhow::Result<()>>>,
) {
    collector_cancel.cancel();
    if let Some(handle) = monitor.take() {
        handle.abort();
    }
    if let Some(handle) = writer.take() {
        handle.abort();
    }
}

/// 启动弹幕采集与互动归档 writer。失败时把快照标记为不可用并降级为仅录制视频。
#[allow(clippy::too_many_arguments)]
pub(super) async fn start_danmu_capture(
    bili_api: &Arc<BiliApi>,
    room_id: i64,
    cookies: &str,
    capture_mode: CaptureMode,
    title: &str,
    started_at: chrono::DateTime<chrono::Utc>,
    paths: &InteractionPaths,
    snapshot: &Arc<Mutex<RecordingInfo>>,
    recent: &Arc<Mutex<VecDeque<super::ArchivedLiveEvent>>>,
    segment_index: &Arc<AtomicU32>,
    collector_cancel: &CancellationToken,
) -> DanmuStartup {
    let (reload_tx, reload_rx) = mpsc::channel(8);
    let (danmu_failure_tx, danmu_failure_rx) = mpsc::channel(1);
    let (danmu_collector_tx, danmu_collector_rx) = mpsc::channel(1);
    let mut startup = DanmuStartup {
        monitor: None,
        writer: None,
        collector_rx: danmu_collector_rx,
        failure_rx: danmu_failure_rx,
        reload_rx,
        persisted_path: None,
        channel_open: false,
        unavailable: false,
    };
    if capture_mode == CaptureMode::Off {
        return startup;
    }
    match bili_api.live_danmu_conf(room_id, cookies).await {
        Ok(danmu_conf) => {
            let hosts: Vec<(String, i32)> = danmu_conf
                .host_server_list
                .iter()
                .map(|host| (host.host.clone(), host.wss_port))
                .collect();
            match DanmuCollector::start(
                room_id,
                danmu_conf.token,
                hosts,
                bili_api.clone(),
                cookies.to_owned(),
                collector_cancel.clone(),
            )
            .await
            {
                Ok((danmu_collector_handle, mut danmu_rx)) => {
                    startup.channel_open = true;
                    startup.monitor = Some(tokio::spawn(async move {
                        match danmu_collector_handle.await {
                            Ok(()) => {
                                // worker 已结束时发送失败无碍：事件本就无人消费。
                                if let Err(error) =
                                    danmu_collector_tx.send(DanmuCollectorEvent::Exited).await
                                {
                                    debug!(
                                        room_id,
                                        "弹幕退出事件发送失败（worker 已结束）: {error}"
                                    );
                                }
                            }
                            Err(error) => {
                                if let Err(send_error) = danmu_collector_tx
                                    .send(DanmuCollectorEvent::Failed(error.to_string()))
                                    .await
                                {
                                    debug!(
                                        room_id,
                                        "弹幕失败事件发送失败（worker 已结束）: {send_error}"
                                    );
                                }
                            }
                        }
                    }));
                    startup.persisted_path = Some(paths.legacy.clone());
                    let writer_args = InteractionWriterArgs {
                        room_id,
                        title: title.to_owned(),
                        mode: capture_mode,
                        started_at,
                        paths: paths.clone(),
                        snapshot: snapshot.clone(),
                        recent: recent.clone(),
                        reload_tx,
                        segment_index: segment_index.clone(),
                    };
                    startup.writer = Some(tokio::spawn(async move {
                        let result = interactions::run(&mut danmu_rx, writer_args).await;
                        if let Err(error) = &result {
                            if let Err(send_error) = danmu_failure_tx.send(error.to_string()).await
                            {
                                debug!(
                                    room_id,
                                    "弹幕写入失败通知发送失败（worker 已结束）: {send_error}"
                                );
                            }
                        }
                        result
                    }));
                }
                Err(error) => {
                    startup.unavailable = true;
                    let mut state = snapshot.lock().await;
                    state.danmu_unavailable = true;
                    state.interaction_capture_status = "unavailable".to_owned();
                    state.interaction_error = Some(error.to_string());
                    tracing::warn!(room_id, "弹幕采集启动失败，降级为仅录制视频: {error}");
                }
            }
        }
        Err(error) => {
            startup.unavailable = true;
            let mut state = snapshot.lock().await;
            state.danmu_unavailable = true;
            state.interaction_capture_status = "unavailable".to_owned();
            state.interaction_error = Some(error.to_string());
            tracing::warn!(room_id, "获取弹幕配置失败，降级为仅录制视频: {error}");
        }
    }
    if !startup.unavailable {
        snapshot.lock().await.interaction_capture_status = "capturing".to_owned();
    }
    startup
}
