from flask import request, jsonify
from . import task_bp
from models import db, Blogger, Log
from services.bili_api import get_bili_api
from datetime import datetime, timedelta
import random
import threading

# 导入 WebSocket 广播函数
def broadcast_log(uid, message, level='info'):
    """广播日志消息"""
    try:
        from app import broadcast_log as _broadcast_log
        _broadcast_log(uid, message, level)
    except:
        pass

# 监控任务管理器
class MonitorManager:
    def __init__(self):
        self.running = False
        self.active_tasks = {}
        self.lock = threading.Lock()
        self.cookies_cache = {}  # 缓存每个博主的cookies
    
    def start_blogger(self, blogger_id, cookies):
        """启动单个博主监控"""
        blogger = Blogger.query.get(blogger_id)
        if not blogger:
            return {'success': False, 'message': '未找到该博主'}

        if blogger.is_running:
            return {'success': False, 'message': '该博主监控已在运行中'}

        with self.lock:
            blogger.is_running = True
            # 缓存cookies供监控服务使用
            if cookies:
                self.cookies_cache[blogger.uid] = cookies
            # 设置下次检查时间（立即执行第一次检查）
            blogger.next_check = datetime.now()
            db.session.commit()

            # 添加日志
            self._add_log(blogger.uid, f'监控已启动，准备执行第一次检查...', 'success')

        return {
            'success': True,
            'message': '监控已启动',
            'next_check': int(blogger.next_check.timestamp())
        }
    
    def stop_blogger(self, blogger_id):
        """停止单个博主监控"""
        blogger = Blogger.query.get(blogger_id)
        if not blogger:
            return {'success': False, 'message': '未找到该博主'}

        if not blogger.is_running:
            return {'success': False, 'message': '该博主监控未在运行'}

        with self.lock:
            blogger.is_running = False
            blogger.next_check = None
            # 清除cookies缓存
            if blogger.uid in self.cookies_cache:
                del self.cookies_cache[blogger.uid]
            db.session.commit()

            # 添加日志
            self._add_log(blogger.uid, '监控已停止', 'info')

        return {'success': True, 'message': '监控已停止'}
    
    def start_all(self, cookies):
        """启动全部监控"""
        bloggers = Blogger.query.all()
        count = 0

        for blogger in bloggers:
            if not blogger.is_running:
                blogger.is_running = True
                # 缓存cookies
                if cookies:
                    self.cookies_cache[blogger.uid] = cookies
                # 立即执行第一次检查
                blogger.next_check = datetime.now()
                count += 1

                # 添加日志
                self._add_log(blogger.uid, '批量启动：监控已开始', 'success')

        db.session.commit()

        # 添加系统日志
        self._add_log(None, f'全部任务已启动，共 {count} 个博主', 'success')

        return {
            'success': True,
            'message': '全部任务已启动',
            'started_count': count
        }
    
    def stop_all(self):
        """停止全部监控"""
        bloggers = Blogger.query.filter_by(is_running=True).all()

        for blogger in bloggers:
            blogger.is_running = False
            blogger.next_check = None
            # 清除cookies缓存
            if blogger.uid in self.cookies_cache:
                del self.cookies_cache[blogger.uid]

            # 添加日志
            self._add_log(blogger.uid, '批量停止：监控已停止', 'info')

        db.session.commit()

        # 添加系统日志
        self._add_log(None, '全部任务已停止', 'info')

        return {'success': True, 'message': '全部任务已停止'}
    
    def get_status(self):
        """获取任务状态"""
        active_count = Blogger.query.filter_by(is_running=True).count()
        
        # 获取下次检查时间（所有运行中任务的最早时间）
        next_check = db.session.query(db.func.min(Blogger.next_check)).filter(
            Blogger.is_running == True
        ).scalar()
        
        return {
            'success': True,
            'running': active_count > 0,
            'server_timestamp': int(datetime.now().timestamp()),
            'next_check_timestamp': int(next_check.timestamp()) if next_check else 0,
            'active_tasks': active_count
        }
    
    def get_next_check(self):
        """获取各博主的下次检查时间"""
        bloggers = Blogger.query.all()
        result = {}
        
        for blogger in bloggers:
            result[blogger.uid] = {
                'uid': blogger.uid,
                'next_check': int(blogger.next_check.timestamp()) if blogger.next_check else 0,
                'is_running': blogger.is_running
            }
        
        return {'success': True, 'bloggers': result}
    
    def _add_log(self, uid, message, level='info'):
        """添加日志"""
        try:
            log = Log(uid=uid, message=message, level=level)
            db.session.add(log)
            db.session.commit()
            
            # 尝试清理日志
            try:
                Log.cleanup_logs()
            except:
                pass

            # 广播日志到所有连接的客户端
            broadcast_log(uid, message, level)
        except Exception as e:
            print(f"添加日志失败: {e}")
            db.session.rollback()

# 全局监控管理器实例
monitor_manager = MonitorManager()

@task_bp.route('/start', methods=['POST'])
def start_task():
    """启动单个博主监控"""
    data = request.get_json()
    
    uid = data.get('uid', '').strip()
    cookies = data.get('cookies', '').strip()
    
    if not uid:
        return jsonify({'success': False, 'message': '请提供博主UID'})
    
    # 查找博主
    blogger = Blogger.query.filter_by(uid=uid).first()
    if not blogger:
        return jsonify({'success': False, 'message': '未找到该博主，请先添加'})
    
    result = monitor_manager.start_blogger(blogger.id, cookies)
    return jsonify(result)

@task_bp.route('/stop', methods=['POST'])
def stop_task():
    """停止单个博主监控"""
    data = request.get_json()
    
    uid = data.get('uid', '').strip()
    
    if not uid:
        return jsonify({'success': False, 'message': '请提供博主UID'})
    
    # 查找博主
    blogger = Blogger.query.filter_by(uid=uid).first()
    if not blogger:
        return jsonify({'success': False, 'message': '未找到该博主'})
    
    result = monitor_manager.stop_blogger(blogger.id)
    return jsonify(result)

@task_bp.route('/start_all', methods=['POST'])
def start_all_tasks():
    """启动全部监控"""
    data = request.get_json() or {}
    cookies = data.get('cookies', '').strip()
    
    result = monitor_manager.start_all(cookies)
    return jsonify(result)

@task_bp.route('/stop_all', methods=['POST'])
def stop_all_tasks():
    """停止全部监控"""
    result = monitor_manager.stop_all()
    return jsonify(result)

@task_bp.route('/status', methods=['GET'])
def get_status():
    """获取任务状态"""
    result = monitor_manager.get_status()
    return jsonify(result)

@task_bp.route('/next_check', methods=['GET'])
def get_next_check():
    """获取下次检查时间"""
    result = monitor_manager.get_next_check()
    return jsonify(result)
