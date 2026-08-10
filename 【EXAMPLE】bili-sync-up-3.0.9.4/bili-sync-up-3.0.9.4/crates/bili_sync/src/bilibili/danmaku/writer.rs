use std::path::PathBuf;

use anyhow::Result;
use tokio::fs::{self, File, OpenOptions};

use crate::bilibili::danmaku::canvas::{CanvasConfig, DanmakuOption};
use crate::bilibili::danmaku::{AssWriter, Danmu};
use crate::bilibili::PageInfo;

pub struct DanmakuWriter<'a> {
    page: &'a PageInfo,
    danmaku: Vec<Danmu>,
}

impl<'a> DanmakuWriter<'a> {
    pub fn new(page: &'a PageInfo, danmaku: Vec<Danmu>) -> Self {
        DanmakuWriter { page, danmaku }
    }

    pub async fn write(self, path: PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let file = File::create(path).await?;
        self.write_inner(file, true).await
    }

    pub async fn append(self, path: PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut write_header = true;
        if let Ok(metadata) = fs::metadata(&path).await {
            write_header = metadata.len() == 0;
        }
        let file = OpenOptions::new().create(true).append(true).open(path).await?;
        self.write_inner(file, write_header).await
    }

    async fn write_inner(self, file: File, write_header: bool) -> Result<()> {
        let canvas_config = canvas_config(self.page);
        let mut writer = if write_header {
            AssWriter::construct(file, self.page.name.clone(), canvas_config.clone()).await?
        } else {
            AssWriter::new(file, self.page.name.clone(), canvas_config.clone())
        };
        let mut canvas = canvas_config.canvas();
        for danmuku in self.danmaku {
            if let Some(drawable) = canvas.draw(danmuku)? {
                writer.write(drawable).await?;
            }
        }
        writer.flush().await?;
        Ok(())
    }
}

fn canvas_config(page: &PageInfo) -> CanvasConfig {
    crate::config::with_config(|bundle| {
        let danmaku_option = bundle.config.danmaku_option.clone();
        let static_option: &'static DanmakuOption = Box::leak(Box::new(danmaku_option));
        CanvasConfig::new(static_option, page)
    })
}
