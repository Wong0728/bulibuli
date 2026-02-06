from flask_sqlalchemy import SQLAlchemy
from sqlalchemy import event
from datetime import datetime
import json

db = SQLAlchemy()

def setup_db_events(app):
    """初始化数据库事件监听"""
    with app.app_context():
        # 开启 SQLite WAL 模式，提升高并发写入性能
        @event.listens_for(db.engine, "connect")
        def set_sqlite_pragma(dbapi_connection, connection_record):
            cursor = dbapi_connection.cursor()
            cursor.execute("PRAGMA journal_mode=WAL")
            cursor.execute("PRAGMA synchronous=NORMAL")
            cursor.close()

class Blogger(db.Model):
    __tablename__ = 'bloggers'

    id = db.Column(db.Integer, primary_key=True)
    uid = db.Column(db.String(20), unique=True, nullable=False)
    name = db.Column(db.String(100), nullable=True)
    min_interval = db.Column(db.Integer, default=60)
    max_interval = db.Column(db.Integer, default=300)
    is_running = db.Column(db.Boolean, default=False)
    next_check = db.Column(db.DateTime, nullable=True)
    created_at = db.Column(db.DateTime, default=datetime.now)
    updated_at = db.Column(db.DateTime, default=datetime.now, onupdate=datetime.now)
    
    def to_dict(self):
        return {
            'id': self.id,
            'uid': self.uid,
            'name': self.name,
            'min_interval': self.min_interval,
            'max_interval': self.max_interval,
            'is_running': self.is_running,
            'next_check': int(self.next_check.timestamp()) if self.next_check else 0,
            'created_at': self.created_at.isoformat() if self.created_at else None,
            'updated_at': self.updated_at.isoformat() if self.updated_at else None
        }

class History(db.Model):
    __tablename__ = 'history'
    
    id = db.Column(db.Integer, primary_key=True)
    uid = db.Column(db.String(20), nullable=True, index=True)  # 关联的博主UID
    bvid = db.Column(db.String(20), nullable=False)
    title = db.Column(db.String(500), nullable=True)
    pub_date = db.Column(db.String(20), nullable=True)
    download_time = db.Column(db.DateTime, default=datetime.now)
    file_path = db.Column(db.String(500), nullable=True)
    
    def to_dict(self):
        return {
            'id': self.id,
            'uid': self.uid,
            'bvid': self.bvid,
            'title': self.title,
            'pub_date': self.pub_date,
            'download_time': self.download_time.strftime('%Y-%m-%d %H:%M:%S') if self.download_time else None,
            'file_path': self.file_path
        }

    @classmethod
    def cleanup_history(cls):
        """自动清理下载历史记录"""
        try:
            # 获取限制
            limit = 1000
            try:
                # 动态获取设置
                from models import Setting
                settings = Setting.get_all_settings()
                limit = settings.get('storage').get('history_limit', 1000)
            except:
                pass
            
            # 如果总数超过限制的 1.1 倍，则清理到限制值
            count = cls.query.count()
            if count > int(limit * 1.1):
                # 获取需要保留的 ID (最新的 limit 条)
                subquery = db.session.query(cls.id).order_by(cls.download_time.desc()).limit(limit).subquery()
                # 删除不在保留列表中的记录
                cls.query.filter(cls.id.notin_(subquery)).delete(synchronize_session=False)
                db.session.commit()
                print(f"[History] 已清理历史记录，保留最新的 {limit} 条")
        except Exception as e:
            print(f"历史记录清理失败: {e}")

class DownloadTask(db.Model):
    __tablename__ = 'download_tasks'
    
    id = db.Column(db.Integer, primary_key=True)
    bvid = db.Column(db.String(20), nullable=False)
    title = db.Column(db.String(500), nullable=True)
    url = db.Column(db.String(1000), nullable=True)
    cookies = db.Column(db.Text, nullable=True)
    quality = db.Column(db.Integer, default=80)
    type = db.Column(db.String(20), default='video')  # video, audio
    status = db.Column(db.String(20), default='pending')  # pending, downloading, completed, failed, paused
    error = db.Column(db.Text, nullable=True)
    progress_percent = db.Column(db.Integer, default=0)
    downloaded_size = db.Column(db.BigInteger, default=0)
    total_size = db.Column(db.BigInteger, default=0)
    speed = db.Column(db.Integer, default=0)
    filename = db.Column(db.String(500), nullable=True)
    gid = db.Column(db.String(20), nullable=True)  # Aria2 任务 ID
    created_at = db.Column(db.DateTime, default=datetime.now)
    updated_at = db.Column(db.DateTime, default=datetime.now, onupdate=datetime.now)
    
    # 复合唯一约束：同一个 BV 号可以同时存在 video 和 audio 两种类型
    __table_args__ = (
        db.UniqueConstraint('bvid', 'type', name='uix_bvid_type'),
    )
    
    def to_dict(self):
        return {
            'id': self.id,
            'bvid': self.bvid,
            'title': self.title,
            'status': self.status,
            'error': self.error,
            'progress_percent': self.progress_percent,
            'downloaded_size': self.downloaded_size,
            'total_size': self.total_size,
            'speed': self.speed,
            'filename': self.filename,
            'gid': self.gid,
            'updated_at': self.updated_at.strftime('%Y-%m-%d %H:%M:%S') if self.updated_at else None
        }

class Setting(db.Model):
    __tablename__ = 'settings'
    
    key = db.Column(db.String(100), primary_key=True)
    value = db.Column(db.Text, nullable=True)
    updated_at = db.Column(db.DateTime, default=datetime.now, onupdate=datetime.now)
    
    @staticmethod
    def get_default_settings():
        return {
            'query': {
                'manual_query_limit': 10,
                'auto_query_limit': 3,
                'video_quality': 112,
                'video_format': 4048
            },
            'parallel_download': {
                'max_parallel': 3,
                'wait_slot_timeout': 300
            },
            'aria2_rpc': {
                'use_rpc': True,  # 默认使用 RPC 连接外部 Aria2 服务
                'host': 'localhost',
                'port': 6800,
                'secret': ''
            },
            'aria2c_basic': {
                'max_connection_per_server': 16,
                'split': 16,
                'min_split_size': '10M',
                'max_tries': 5,
                'retry_wait': 5,
                'max_concurrent_downloads': 3
            },
            'storage': {
                'history_limit': 1000,
                'uid_history_limit': 10,
                'log_limit': 100
            },
            'download_path': {
                'base_path': '',  # 空字符串表示使用默认downloads目录
                'auto_organize': True,  # 是否按博主UID自动分文件夹
                'path_template': '{blogger_uid}/{title}'  # 路径模板
            }
        }
    
    @classmethod
    def get_all_settings(cls):
        """获取合并了默认值的所有设置"""
        # 以默认设置作为基础
        settings = cls.get_default_settings()
        
        # 从数据库加载已保存的设置并覆盖默认值
        try:
            db_settings = cls.query.all()
            for setting in db_settings:
                try:
                    val = json.loads(setting.value)
                    # 如果是字典类型且默认配置中也存在该分类，则进行合并
                    if isinstance(val, dict) and setting.key in settings and isinstance(settings[setting.key], dict):
                        settings[setting.key].update(val)
                    else:
                        settings[setting.key] = val
                except:
                    settings[setting.key] = setting.value
        except Exception as e:
            print(f"加载设置失败: {e}")
            
        return settings
    
    @classmethod
    def get_setting(cls, key, default=None):
        setting = cls.query.filter_by(key=key).first()
        if setting:
            try:
                import json
                return json.loads(setting.value)
            except:
                return setting.value
        return default
    
    @classmethod
    def set_setting(cls, key, value):
        import json
        setting = cls.query.filter_by(key=key).first()
        value_str = json.dumps(value) if not isinstance(value, str) else value
        if setting:
            setting.value = value_str
        else:
            setting = cls(key=key, value=value_str)
            db.session.add(setting)
        db.session.commit()

class Log(db.Model):
    __tablename__ = 'logs'
    
    id = db.Column(db.Integer, primary_key=True)
    level = db.Column(db.String(20), default='info')  # info, success, error, warning
    message = db.Column(db.Text, nullable=False)
    uid = db.Column(db.String(20), nullable=True, index=True)  # 博主UID，为空表示系统日志
    created_at = db.Column(db.DateTime, default=datetime.now)
    
    def to_dict(self):
        return {
            'id': self.id,
            'level': self.level,
            'msg': self.message,
            'time': self.created_at.strftime('%H:%M:%S') if self.created_at else None
        }

    @classmethod
    def cleanup_logs(cls):
        """自动清理日志"""
        try:
            # 获取限制
            limit = 100
            try:
                settings = Setting.get_all_settings()
                limit = settings.get('storage').get('log_limit')
            except:
                pass
            
            # 简单的清理策略：如果总数超过限制的 1.2 倍，则清理到限制值
            # 避免每次都执行删除操作
            count = cls.query.count()
            if count > int(limit * 1.2):
                # 获取需要保留的 ID
                subquery = db.session.query(cls.id).order_by(cls.created_at.desc()).limit(limit).subquery()
                # 删除不在保留列表中的日志
                cls.query.filter(cls.id.notin_(subquery)).delete(synchronize_session=False)
                db.session.commit()
        except Exception as e:
            print(f"日志清理失败: {e}")
