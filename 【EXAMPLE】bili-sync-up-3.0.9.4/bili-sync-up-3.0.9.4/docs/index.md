---
# https://vitepress.dev/reference/default-theme-home-page
layout: home

title: bili-sync
titleTemplate: 由 Rust & Tokio 驱动的哔哩哔哩同步工具（升级优化版）

hero:
  name: "bili-sync-up"
  text: "由 Rust & Tokio 驱动的哔哩哔哩同步工具（升级优化版）"
  actions:
    - theme: brand
      text: 什么是 bili-sync？
      link: /introduction
    - theme: alt
      text: 快速开始
      link: /quick-start
    - theme: alt
      text: GitHub
      link: https://github.com/NeeYoonc/bili-sync-up
  image:
    src: /logo.webp
    alt: bili-sync

features:
  - icon: 🎯
    title: 87007智能处理
    details: 自动识别并处理充电专享视频，零干预体验，彻底解决重复错误问题
  - icon: 🛡️
    title: 任务队列持久化
    details: 任务状态保存到数据库，程序重启后自动恢复，保证企业级稳定性
  - icon: 🔍
    title: 失败任务智能筛选
    details: 一键筛选所有失败任务，快速定位问题，提升故障排查效率
  - icon: 🔥
    title: 配置热重载系统
    details: 配置完全基于数据库，支持实时热重载，无需重启即可生效
  - icon: 💾
    title: 专为 NAS 设计
    details: 可被 Emby、Jellyfin 等媒体服务器一键识别，完整的元数据支持
  - icon: 🐳
    title: 部署简单
    details: 提供简单易用的 docker 镜像，支持多架构部署
---

<style>
:root {
  --vp-home-hero-name-color: transparent;
  --vp-home-hero-name-background: -webkit-linear-gradient(120deg, #bd34fe 30%, #41d1ff);

  --vp-home-hero-image-background-image: linear-gradient(-45deg, #bd34fe 50%, #47caff 50%);
  --vp-home-hero-image-filter: blur(44px);
}

@media (min-width: 640px) {
  :root {
    --vp-home-hero-image-filter: blur(56px);
  }
}

@media (min-width: 960px) {
  :root {
    --vp-home-hero-image-filter: blur(68px);
  }
}
</style>