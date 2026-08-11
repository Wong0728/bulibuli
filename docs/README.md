# 文档索引

## 项目与审查

- [审查报告](../AUDIT_REPORT.md)
- [代码规范](../代码规范.md)
- [问题归因与优化方案](问题归因与优化方案.md)
- [AI Skill 使用说明](skill.md)

## 主线迁移与发布

- [Rust v2 主线迁移状态](v2-main-migration-status.md)
- [Alpha Release 安全状态](release-security-status-v2.0.0-alpha.1.md)

## 展示与部署

- [Hero SVG 预览](hero/bulibuli_hero_preview.svg)
- [部署安全说明](../deploy/SECURITY.md)
- [内置资源清单与校验](../resources/README.md)

## 本地工作区边界

以下目录属于本地运行、构建或参考内容，已被 `.gitignore` 排除，不进入云端主线：

- `data/`：本地数据库、日志、下载内容和运行状态；其中 `downloads/` 是用户数据，不自动移动或删除。
- `target/`：Rust 构建缓存和产物；需要释放空间时再单独清理。
- `dist/`、`static/dist/`：本地打包和发布暂存内容；保留到确认不再需要后再清理。
- `static/js/node_modules/`、`__pycache__/`：本地依赖和缓存。
- 根目录旧 `【EXAMPLE】` 目录：本地参考残留，不属于主线；清理前先确认不再需要。
