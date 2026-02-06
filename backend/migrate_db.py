#!/usr/bin/env python3
"""
数据库迁移脚本
将 download_tasks 表的 bvid 唯一约束改为 bvid + type 复合唯一约束
"""
import os
import sys

# 添加 backend 目录到路径
backend_dir = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, backend_dir)

from app import create_app
from models import db
from sqlalchemy import text

def migrate():
    """执行数据库迁移"""
    app = create_app()
    
    with app.app_context():
        # 获取数据库连接
        engine = db.engine
        
        # 检查当前数据库类型
        db_url = str(engine.url)
        print(f"数据库 URL: {db_url}")
        
        if 'sqlite' in db_url:
            # SQLite 迁移
            migrate_sqlite(engine)
        else:
            # 其他数据库（如 MySQL、PostgreSQL）
            migrate_other(engine)
        
        print("迁移完成！")

def migrate_sqlite(engine):
    """SQLite 数据库迁移"""
    from sqlalchemy import inspect
    
    inspector = inspect(engine)
    
    # 检查现有索引
    indexes = inspector.get_indexes('download_tasks')
    print(f"现有索引: {[idx['name'] for idx in indexes]}")
    
    # 检查现有约束
    # SQLite 不支持直接删除唯一约束，需要重建表
    
    with engine.connect() as conn:
        # 1. 创建新表
        conn.execute(text("""
            CREATE TABLE IF NOT EXISTS download_tasks_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                bvid VARCHAR(20) NOT NULL,
                title VARCHAR(500),
                url VARCHAR(1000),
                cookies TEXT,
                quality INTEGER DEFAULT 80,
                type VARCHAR(20) DEFAULT 'video',
                status VARCHAR(20) DEFAULT 'pending',
                error TEXT,
                progress_percent INTEGER DEFAULT 0,
                downloaded_size BIGINT DEFAULT 0,
                total_size BIGINT DEFAULT 0,
                speed INTEGER DEFAULT 0,
                filename VARCHAR(500),
                gid VARCHAR(20),
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                UNIQUE (bvid, type)
            )
        """))
        
        # 2. 复制数据
        conn.execute(text("""
            INSERT INTO download_tasks_new 
            SELECT * FROM download_tasks
        """))
        
        # 3. 删除旧表
        conn.execute(text("DROP TABLE download_tasks"))
        
        # 4. 重命名新表
        conn.execute(text("ALTER TABLE download_tasks_new RENAME TO download_tasks"))
        
        conn.commit()
        print("SQLite 表结构已更新")

def migrate_other(engine):
    """其他数据库迁移（MySQL、PostgreSQL 等）"""
    with engine.connect() as conn:
        # 删除旧的唯一约束
        try:
            conn.execute(text("""
                ALTER TABLE download_tasks 
                DROP INDEX IF EXISTS ix_download_tasks_bvid
            """))
            print("已删除旧索引 ix_download_tasks_bvid")
        except Exception as e:
            print(f"删除旧索引时出错（可能不存在）: {e}")
        
        try:
            conn.execute(text("""
                ALTER TABLE download_tasks 
                DROP CONSTRAINT IF EXISTS uix_bvid_type
            """))
            print("已删除旧约束 uix_bvid_type")
        except Exception as e:
            print(f"删除旧约束时出错（可能不存在）: {e}")
        
        # 添加新的复合唯一约束
        try:
            conn.execute(text("""
                ALTER TABLE download_tasks 
                ADD CONSTRAINT uix_bvid_type 
                UNIQUE (bvid, type)
            """))
            print("已添加新约束 uix_bvid_type")
        except Exception as e:
            print(f"添加新约束时出错: {e}")
        
        conn.commit()

if __name__ == '__main__':
    print("=" * 50)
    print("数据库迁移工具")
    print("=" * 50)
    print("此脚本将修改 download_tasks 表的约束：")
    print("  - 删除 bvid 的唯一约束")
    print("  - 添加 bvid + type 的复合唯一约束")
    print("=" * 50)
    
    # 自动确认（用于自动化脚本）
    import sys
    if '--auto' in sys.argv:
        print("自动模式：跳过确认")
        migrate()
    else:
        confirm = input("是否继续? (yes/no): ")
        if confirm.lower() in ['yes', 'y']:
            migrate()
        else:
            print("已取消迁移")
