from flask import Flask, render_template, send_from_directory
from flask_cors import CORS
from flask_socketio import SocketIO, emit
import os
import sys
from datetime import datetime

# 移除可能干扰打包的路径插入
# sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from config import Config
from models import db, setup_db_events
from routes import video_bp, download_bp, task_bp, blogger_bp, history_bp, settings_bp, logs_bp, cookies_bp
from routes.download import get_download_manager
from services.monitor_service import get_monitor_service

# 创建 SocketIO 实例
# 强制使用 threading 模式，以确保在 Windows 打包环境下的最大兼容性，避免 eventlet/gevent 导致的启动失败
socketio = SocketIO(cors_allowed_origins="*", async_mode='threading', logger=False, engineio_logger=False)

def get_resource_path():
    """获取静态资源根目录"""
    if hasattr(sys, '_MEIPASS'):
        return sys._MEIPASS
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def create_app():
    app = Flask(__name__)
    app.config.from_object(Config)

    # 启用CORS - 仅允许本地开发环境，提升安全性
    CORS(app, resources={
        r"/api/*": {
            "origins": ["http://localhost:5000", "http://127.0.0.1:5000"],
            "methods": ["GET", "POST", "OPTIONS"],
            "allow_headers": ["Content-Type", "Authorization"]
        }
    })

    # 初始化 SocketIO
    socketio.init_app(app)

    # 初始化数据库
    db.init_app(app)
    
    # 注册数据库事件
    setup_db_events(app)

    # 注册蓝图
    app.register_blueprint(video_bp)
    app.register_blueprint(download_bp)
    app.register_blueprint(task_bp)
    app.register_blueprint(blogger_bp)
    app.register_blueprint(history_bp)
    app.register_blueprint(settings_bp)
    app.register_blueprint(logs_bp)
    app.register_blueprint(cookies_bp)

    # 确保数据目录存在
    if not os.path.exists(Config.DATA_DIR):
        os.makedirs(Config.DATA_DIR)

    # 确保数据目录存在
    with app.app_context():
        db.create_all()

    # 初始化下载管理器的应用上下文
    get_download_manager().init_app(app)

    # 初始化监控服务
    monitor_service = get_monitor_service()
    monitor_service.init_app(app)
    # 启动监控服务后台线程
    monitor_service.start()

    # 注册 SocketIO 事件处理
    register_socketio_handlers()

    # 静态文件路由 - 服务于前端文件
    @app.route('/')
    def index():
        return send_from_directory(get_resource_path(), 'index.html')

    @app.route('/css/<path:filename>')
    def serve_css(filename):
        return send_from_directory(os.path.join(get_resource_path(), 'css'), filename)

    @app.route('/js/<path:filename>')
    def serve_js(filename):
        return send_from_directory(os.path.join(get_resource_path(), 'js'), filename)

    @app.route('/resources/<path:filename>')
    def serve_resources(filename):
        return send_from_directory(os.path.join(get_resource_path(), 'resources'), filename)

    @app.route('/favicon.ico')
    def serve_favicon():
        """提供网站图标"""
        # 优先查找根目录下的 bilibili.ico
        favicon_path = os.path.join(get_resource_path(), 'bilibili.ico')
        if os.path.exists(favicon_path):
            return send_from_directory(get_resource_path(), 'bilibili.ico')
            
        # 备选：查找 resources 目录下的 favicon.ico
        favicon_path = os.path.join(get_resource_path(), 'resources', 'favicon.ico')
        if os.path.exists(favicon_path):
            return send_from_directory(os.path.join(get_resource_path(), 'resources'), 'favicon.ico')
            
        return '', 404

    # 健康检查
    @app.route('/api/health', methods=['GET'])
    def health_check():
        return {'success': True, 'message': '服务运行正常'}

    return app

def register_socketio_handlers():
    """注册 WebSocket 事件处理器"""

    @socketio.on('connect')
    def handle_connect():
        print('[WebSocket] 客户端已连接')
        emit('connected', {'message': '连接成功'})

    @socketio.on('disconnect')
    def handle_disconnect():
        print('[WebSocket] 客户端已断开')

    @socketio.on('subscribe_blogger_logs')
    def handle_subscribe_blogger_logs(data):
        """订阅博主日志更新"""
        uid = data.get('uid')
        if uid:
            print(f'[WebSocket] 客户端订阅博主 {uid} 的日志')
            emit('subscribed', {'uid': uid, 'message': f'已订阅博主 {uid} 的日志更新'})

    @socketio.on('subscribe_download_progress')
    def handle_subscribe_download_progress():
        """订阅下载进度更新"""
        print('[WebSocket] 客户端订阅下载进度')
        emit('subscribed', {'message': '已订阅下载进度更新'})

# 全局广播函数
def broadcast_log(uid, message, level='info'):
    """广播日志消息到所有客户端"""
    socketio.emit('log_update', {
        'uid': uid,
        'message': message,
        'level': level,
        'time': datetime.now().strftime('%H:%M:%S')
    })

def broadcast_download_progress(bvid, data):
    """广播下载进度更新"""
    socketio.emit('download_progress', {
        'bvid': bvid,
        **data
    })

if __name__ == '__main__':
    from datetime import datetime
    app = create_app()
    socketio.run(app, host='0.0.0.0', port=5000, debug=True, allow_unsafe_werkzeug=True)
