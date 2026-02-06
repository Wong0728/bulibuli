import os
import sys

def get_base_dir():
    """获取程序的基础目录"""
    if hasattr(sys, '_MEIPASS'):
        # 打包后的内部资源目录
        return sys._MEIPASS
    return os.path.dirname(os.path.abspath(__file__))

def get_app_root():
    """获取应用程序的根目录（打包后为 .exe 所在目录）"""
    if hasattr(sys, '_MEIPASS'):
        # 打包后，返回 .exe 所在的目录
        return os.path.dirname(sys.executable)
    # 开发模式，返回项目根目录（backend 的上级目录）
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

class Config:
    BASE_DIR = get_base_dir()
    APP_ROOT = get_app_root()
    
    # 数据目录 - 打包后应位于 .exe 旁，而不是资源目录内
    DATA_DIR = os.path.join(APP_ROOT, 'data')
    
    SECRET_KEY = os.environ.get('SECRET_KEY') or 'your-secret-key-here-change-in-production'
    
    # 数据库配置 - 使用WAL模式提高并发性能
    SQLALCHEMY_DATABASE_URI = os.environ.get('DATABASE_URL') or f'sqlite:///{os.path.join(DATA_DIR, "app.db")}?mode=rwc'
    SQLALCHEMY_TRACK_MODIFICATIONS = False
    
    # 连接池配置
    SQLALCHEMY_ENGINE_OPTIONS = {
        'connect_args': {
            'check_same_thread': False,  # 允许跨线程使用连接
            'timeout': 30,  # 连接超时时间（秒）
        },
        'pool_pre_ping': True,  # 连接前ping测试
        'pool_recycle': 3600,  # 连接回收时间
    }
    
    # 下载配置 - 存储在数据目录下的 downloads 子目录
    DOWNLOAD_DIR = os.path.join(DATA_DIR, 'downloads')
    MAX_PARALLEL_DOWNLOADS = 3
    
    # 监控配置
    CHECK_INTERVAL_MIN = 60  # 最小检查间隔（秒）
    CHECK_INTERVAL_MAX = 300  # 最大检查间隔（秒）
    
    # B站API配置
    BILI_API_TIMEOUT = 10
    USER_AGENT = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
    REFERER = 'https://www.bilibili.com/'
