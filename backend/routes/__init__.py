from flask import Blueprint

# 创建蓝图
video_bp = Blueprint('video', __name__, url_prefix='/api/video')

task_bp = Blueprint('task', __name__, url_prefix='/api/task')
blogger_bp = Blueprint('blogger', __name__, url_prefix='/api/blogger')
history_bp = Blueprint('history', __name__, url_prefix='/api/history')
settings_bp = Blueprint('settings', __name__, url_prefix='/api/settings')
logs_bp = Blueprint('logs', __name__, url_prefix='/api/logs')
cookies_bp = Blueprint('cookies', __name__, url_prefix='/api/cookies')

# 导入路由
from . import video, task, blogger, history, settings, logs, cookies
from .download import download_bp
