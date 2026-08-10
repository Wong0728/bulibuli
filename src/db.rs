//! 数据库模块：连接初始化（`init`）与迁移执行（`migrations`）。

mod init;
mod migrations;

pub use init::init_database;
