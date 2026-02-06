import threading
import time
import random
from datetime import datetime, timedelta
from models import db, Blogger, Log, History, DownloadTask
from services.bili_api import get_bili_api

# 导入 WebSocket 广播函数（循环导入处理）
def broadcast_log(uid, message, level='info'):
    """广播日志消息"""
    try:
        from app import broadcast_log as _broadcast_log
        _broadcast_log(uid, message, level)
    except:
        pass

class MonitorService:
    """监控服务 - 真正执行监测任务的后台服务"""
    
    def __init__(self):
        self.running = False
        self.thread = None
        self.lock = threading.Lock()
        self.check_interval = 10  # 每10秒检查一次是否有任务需要执行
        self.app = None
        
    def init_app(self, app):
        """初始化应用上下文"""
        self.app = app
        
    def start(self):
        """启动监控服务"""
        with self.lock:
            if self.running:
                return
            self.running = True
            self.thread = threading.Thread(target=self._monitor_loop, daemon=True)
            self.thread.start()
            print("[MonitorService] 监控服务已启动")
            
    def stop(self):
        """停止监控服务"""
        with self.lock:
            self.running = False
            print("[MonitorService] 监控服务已停止")
            
    def _monitor_loop(self):
        """监控主循环"""
        print("[MonitorService] 监控循环已启动")
        while self.running:
            try:
                if self.app:
                    with self.app.app_context():
                        self._check_and_execute_tasks()
                time.sleep(self.check_interval)
            except Exception as e:
                print(f"[MonitorService] 监控循环出错: {e}")
                import traceback
                traceback.print_exc()
                time.sleep(self.check_interval)
        print("[MonitorService] 监控循环已停止")
                
    def _check_and_execute_tasks(self):
        """检查并执行需要运行的任务"""
        now = datetime.now()

        # 查找所有需要执行检查的运行中博主
        bloggers = Blogger.query.filter(
            Blogger.is_running == True,
            Blogger.next_check <= now
        ).all()

        if bloggers:
            print(f"[MonitorService] 发现 {len(bloggers)} 个博主需要检查")

        for blogger in bloggers:
            try:
                print(f"[MonitorService] 执行检查: 博主 {blogger.uid}")
                self._execute_check(blogger)
            except Exception as e:
                print(f"[MonitorService] 检查博主 {blogger.uid} 失败: {e}")
                self._add_log(blogger.uid, f"检查失败: {str(e)}", "error")
                
    def _get_cookies_for_blogger(self, uid):
        """获取系统保存的cookies"""
        # 从系统设置中读取cookies
        try:
            from models import Setting
            cookies = Setting.get_setting('cookies', '')
            return cookies if cookies else ''
        except Exception as e:
            print(f"[MonitorService] 获取cookies失败: {e}")
            return ''

    def _execute_check(self, blogger):
        """执行单个博主的检查"""
        uid = blogger.uid
        print(f"[MonitorService] 开始检查博主: {uid}")

        # 获取cookies
        cookies = self._get_cookies_for_blogger(uid)

        if not cookies:
            self._add_log(uid, f"错误: 未配置Cookies，无法获取视频", "error")
            self._schedule_next_check(blogger)
            return

        # 添加日志
        self._add_log(uid, f"开始检查博主 {uid} 的新视频...", "info")

        # 获取设置
        from models import Setting
        settings = Setting.get_all_settings()
        query_limit = settings.get('query').get('auto_query_limit')

        # 调用B站API获取视频列表
        try:
            result = get_bili_api().get_user_videos(uid, cookies=cookies, limit=query_limit)

            if not result.get('success'):
                error_msg = result.get('message', '获取视频列表失败')
                self._add_log(uid, f"获取视频列表失败: {error_msg}", "error")
                # 设置下次检查时间（即使失败也继续）
                self._schedule_next_check(blogger)
                return

            videos = result.get('videos', [])
            self._add_log(uid, f"获取到 {len(videos)} 个视频", "info")

            # 检查每个视频是否已下载
            new_videos = []
            for video in videos:
                bvid = video.get('bvid')
                if not bvid:
                    continue

                # 检查是否已在历史中
                existing = History.query.filter_by(bvid=bvid).first()
                if not existing:
                    new_videos.append(video)

            if new_videos:
                self._add_log(uid, f"发现 {len(new_videos)} 个新视频!", "success")
                # 自动处理所有新视频
                for video in new_videos:
                    self._add_to_download_queue(uid, video, cookies)
            else:
                self._add_log(uid, "没有新视频", "info")

            # 设置下次检查时间
            self._schedule_next_check(blogger)

        except Exception as e:
            self._add_log(uid, f"检查过程出错: {str(e)}", "error")
            self._schedule_next_check(blogger)
            
    def _schedule_next_check(self, blogger):
        """设置下次检查时间"""
        interval = random.randint(blogger.min_interval, blogger.max_interval)
        blogger.next_check = datetime.now() + timedelta(seconds=interval)
        db.session.commit()

        next_time_str = blogger.next_check.strftime("%H:%M:%S")
        self._add_log(blogger.uid, f"下次检查时间: {next_time_str} (间隔 {interval} 秒)", "info")
        
    def _add_to_download_queue(self, uid, video, cookies=None):
        """添加视频到下载队列"""
        bvid = video.get('bvid')
        title = video.get('title', '未知标题')

        # 检查是否已在下载队列（只检查视频类型）
        existing = DownloadTask.query.filter_by(bvid=bvid, type='video').first()
        if existing:
            self._add_log(uid, f"视频 {title} 已在下载队列中", "info")
            return

        # 检查是否已在历史记录中（避免重复处理）
        existing_history = History.query.filter_by(bvid=bvid).first()
        if existing_history:
            self._add_log(uid, f"视频 {title} 已下载过，跳过", "info")
            return

        # 获取视频下载链接
        try:
            from models import Setting
            settings = Setting.get_all_settings()
            video_quality = settings.get('query').get('video_quality')

            # 获取视频URL信息
            url_result = get_bili_api().get_video_urls(bvid, cookies=cookies, fnval=16)
            if url_result.get('success') and url_result.get('qualities'):
                # 选择合适的清晰度
                qualities = url_result.get('qualities', [])
                selected_quality = None
                for q in qualities:
                    if q.get('quality') <= video_quality:
                        selected_quality = q
                        break
                if not selected_quality and qualities:
                    selected_quality = qualities[0]

                if selected_quality:
                    # 立即触发下载管理器处理视频任务
                    # 注意：DownloadManager.add_task 会自动处理对应的音频任务，
                    # 并且会处理数据库记录的创建，所以这里不需要手动创建 DownloadTask
                    try:
                        from routes.download import get_download_manager
                        result = get_download_manager().add_task(
                            bvid=bvid,
                            title=title,
                            url=selected_quality.get('url'),
                            cookies=cookies,
                            quality=selected_quality.get('quality', 80),
                            task_type='video',
                            uid=uid  # 传递博主UID用于自动分文件夹
                        )
                        
                        if result.get('success'):
                            self._add_log(uid, f"已添加视频到下载队列: {title} ({selected_quality.get('quality_name', '未知清晰度')})", "success")
                        else:
                            self._add_log(uid, f"添加视频下载任务失败: {result.get('message')}", "error")
                    except Exception as e:
                        self._add_log(uid, f"添加视频下载任务出错: {str(e)}", "error")
                else:
                    self._add_log(uid, f"视频 {title} 未找到合适的清晰度", "warning")
            else:
                self._add_log(uid, f"视频 {title} 获取下载链接失败: {url_result.get('message', '未知错误')}", "warning")
        except Exception as e:
            self._add_log(uid, f"处理视频 {title} 出错: {str(e)}", "error")
        
    def _add_log(self, uid, message, level='info'):
        """添加日志并广播"""
        try:
            log = Log(uid=uid, message=message, level=level)
            db.session.add(log)
            db.session.commit()
            
            # 尝试清理日志
            try:
                Log.cleanup_logs()
            except:
                pass
            
            print(f"[MonitorService] [{level}] {message}")

            # 广播日志到所有连接的客户端
            broadcast_log(uid, message, level)
        except Exception as e:
            print(f"添加日志失败: {e}")
            db.session.rollback()

# 全局监控服务实例
_monitor_service = None

def get_monitor_service() -> MonitorService:
    """获取全局监控服务实例"""
    global _monitor_service
    if _monitor_service is None:
        _monitor_service = MonitorService()
    return _monitor_service
