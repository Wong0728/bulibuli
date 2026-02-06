from flask import request, jsonify, current_app, Blueprint

download_bp = Blueprint('download', __name__, url_prefix='/api/download')

from config import Config
from models import db, DownloadTask, History, Setting
from datetime import datetime, timedelta
from urllib.parse import urlparse, parse_qs, quote
from services.bili_api import get_bili_api
from services.aria2_service import Aria2DownloadManager, init_aria2_manager, get_aria2_manager, get_rpc_server
from services.video_processor import VideoProcessor
from utils.helpers import sanitize_filename
import os
import requests
import threading
import time

# 下载管理器 - 使用 Aria2 进行下载
class DownloadManager:
    def __init__(self):
        self._default_download_dir = Config.DOWNLOAD_DIR
        self.download_dir = self._default_download_dir
        os.makedirs(self.download_dir, exist_ok=True)
        self._lock = threading.Lock()
        self._app = None
        self._aria2_manager = None
        self._monitor_thread = None
        self._running = False
        # 内存缓存下载进度，减少数据库写入
        self._progress_cache = {}  # bvid -> {progress_percent, downloaded_size, total_size, speed, last_update}

    def _get_download_dir(self, uid=None):
        """获取下载目录，支持按博主自动分文件夹"""
        # 默认下载目录
        base_path = Config.DOWNLOAD_DIR

        if not self._app:
            # 如果没有应用上下文，使用默认目录
            if uid:
                download_dir = os.path.join(base_path, str(uid))
            else:
                download_dir = base_path
            os.makedirs(download_dir, exist_ok=True)
            return download_dir

        try:
            with self._app.app_context():
                settings = Setting.get_all_settings()
                path_config = settings.get('download_path', {})

                # 是否自动组织文件夹
                auto_organize = path_config.get('auto_organize', True)

                if auto_organize and uid:
                    # 按博主UID创建子目录
                    download_dir = os.path.join(base_path, str(uid))
                else:
                    download_dir = base_path

                # 确保目录存在
                os.makedirs(download_dir, exist_ok=True)
                return download_dir
        except Exception as e:
            print(f"[DownloadManager] 获取下载目录失败: {e}, 使用默认目录")
            if uid:
                download_dir = os.path.join(base_path, str(uid))
            else:
                download_dir = base_path
            os.makedirs(download_dir, exist_ok=True)
            return download_dir

    def init_app(self, app):
        """初始化应用上下文"""
        self._app = app

        # 加载下载路径配置
        with app.app_context():
            # 强制使用全局配置的下载目录
            self.download_dir = Config.DOWNLOAD_DIR
            
            settings = Setting.get_all_settings()
            aria2_config = settings.get('aria2_rpc')
            use_rpc = aria2_config.get('use_rpc')
            host = aria2_config.get('host')
            port = aria2_config.get('port')
            secret = aria2_config.get('secret')

        # 检查外部 Aria2 RPC 服务是否可用（不再启动本地服务）
        rpc_server = get_rpc_server(port, self.download_dir)
        if rpc_server.start():
            print("[DownloadManager] 已连接到外部 Aria2 RPC 服务")
        else:
            print("[DownloadManager] 警告: 无法连接到外部 Aria2 RPC 服务，将使用本地下载作为备用")
            print(f"[DownloadManager] 请确保 Aria2 已启动: aria2c --enable-rpc --rpc-listen-port={port}")

        # 初始化 Aria2 管理器（优先使用RPC，RPC不可用时使用本地下载）
        self._aria2_manager = init_aria2_manager(
            download_dir=self.download_dir,
            use_rpc=True,  # 始终尝试使用RPC
            host=host,
            port=port,
            secret=secret
        )

        # 启动监控线程
        self._running = True
        self._monitor_thread = threading.Thread(target=self._monitor_downloads)
        self._monitor_thread.daemon = True
        self._monitor_thread.start()
    
    def _get_aria2_options(self):
        """获取 Aria2 下载选项"""
        if not self._app:
            return {}

        with self._app.app_context():
            settings = Setting.get_all_settings()
            aria2_basic = settings.get('aria2c_basic', {})

            return {
                # 分片下载配置
                'split': aria2_basic.get('split'),  # 分片数量
                'max_connection_per_server': aria2_basic.get('max_connection_per_server'),  # 每个服务器最大连接数
                'min_split_size': aria2_basic.get('min_split_size'),  # 最小分片大小
                # 重试配置
                'max_tries': aria2_basic.get('max_tries'),  # 最大重试次数
                'retry_wait': aria2_basic.get('retry_wait'),  # 重试等待时间
                # 并发配置
                'max_concurrent_downloads': aria2_basic.get('max_concurrent_downloads'),  # 最大同时下载数
            }
    
    def add_task(self, bvid, title, url, cookies, quality=80, task_type='video', uid=None):
        """添加下载任务到 Aria2

        Args:
            bvid: 视频BV号
            title: 视频标题
            url: 下载链接
            cookies: Cookies字符串
            quality: 视频质量
            task_type: 任务类型
            uid: 博主UID，用于自动分文件夹
        """
        if not self._aria2_manager:
            return {'success': False, 'message': 'Aria2 管理器未初始化'}

        # 检查 Aria2 是否可用
        if not self._aria2_manager.is_available():
            return {'success': False, 'message': 'Aria2 下载器不可用，请确保 aria2c.exe 存在'}

        with self._app.app_context():
            # 检查是否已存在（同时考虑 bvid 和 type）
            existing = DownloadTask.query.filter_by(bvid=bvid, type=task_type).first()
            if existing:
                if existing.status == 'downloading':
                    message = f"该{task_type}正在下载中"
                    return {'success': False, 'message': message}
                elif existing.status == 'completed':
                    # 已下载完成的视频，检查文件是否存在
                    uid_from_history = self._get_blogger_uid_from_history(bvid)
                    download_dir = self._get_download_dir(uid_from_history)
                    ext = 'm4s'
                    filename = f"{bvid}.{ext}"
                    file_path = os.path.join(download_dir, filename)

                    # 如果文件存在，提示用户已下载
                    if os.path.exists(file_path):
                        message = f"该{task_type}已下载完成，文件已存在"
                        return {'success': False, 'message': message}

                    # 文件不存在，删除旧记录重新下载
                    db.session.delete(existing)
                    db.session.commit()
                    # 继续执行下面的新任务创建逻辑
                else:
                    # 重试失败/暂停的任务 - 重新添加到 Aria2
                    existing.status = 'pending'
                    existing.error = None
                    existing.url = url
                    existing.cookies = cookies
                    existing.progress_percent = 0
                    existing.downloaded_size = 0
                    existing.total_size = 0
                    existing.speed = 0
                    existing.gid = None
                    # 强制重置文件名为 BV号.m4s，确保合并逻辑能正常工作
                    # 即使 Aria2 自动重命名为 .1，监控线程也能捕获
                    existing.filename = f"{bvid}.m4s"
                    db.session.commit()

                    # 添加到 Aria2
                    result = self._add_to_aria2(existing, uid)
                    if result['success']:
                        existing.gid = result['gid']
                        existing.status = 'downloading'
                        db.session.commit()
                    return {'success': True, 'message': '已重新添加到下载队列', 'download_id': existing.id}

            # 创建新任务
            # 确定文件扩展名
            ext = 'm4s'  # 默认为 m4s
            if task_type == 'audio':
                ext = 'm4s'
            
            # 初始文件名使用 BVID，以便于合并和管理
            filename = f"{bvid}.{ext}"
            
            task = DownloadTask(
                bvid=bvid,
                title=title,
                url=url,
                cookies=cookies,
                quality=quality,
                type=task_type,
                status='pending',
                filename=filename  # 保存初始文件名
            )
            db.session.add(task)
            db.session.commit()

            # 添加到 Aria2
            result = self._add_to_aria2(task, uid)
            if result['success']:
                task.gid = result['gid']
                task.status = 'downloading'
                db.session.commit()

                # 如果是视频任务，尝试下载对应的音频
                if task_type == 'video':
                    try:
                        # 获取视频信息（需要 cid 来获取音频链接，bili_api.get_audio_url 会自动处理）
                        audio_result = get_bili_api().get_audio_url(bvid, cookies=cookies)
                        if audio_result.get('success'):
                            audio_url = audio_result.get('audio_url')
                            if audio_url:
                                # 检查是否已存在音频任务
                                existing_audio_task = DownloadTask.query.filter_by(bvid=bvid, type='audio').first()
                                if not existing_audio_task:
                                    audio_filename = f"{bvid}.m4s"
                                    audio_task = DownloadTask(
                                        bvid=bvid,
                                        title=title,
                                        url=audio_url,
                                        cookies=cookies,
                                        quality=quality,
                                        type='audio',
                                        status='pending',
                                        filename=audio_filename
                                    )
                                    db.session.add(audio_task)
                                    db.session.commit()

                                    audio_result_aria2 = self._add_to_aria2(audio_task, uid)
                                    if audio_result_aria2['success']:
                                        audio_task.gid = audio_result_aria2['gid']
                                        audio_task.status = 'downloading'
                                        db.session.commit()
                                        print(f"[DownloadManager] 已添加音频下载任务: {title}")
                                    else:
                                        audio_task.status = 'failed'
                                        audio_task.error = audio_result_aria2['message']
                                        db.session.commit()
                                        print(f"[DownloadManager] 添加音频下载任务失败: {audio_result_aria2['message']}")
                                else:
                                    print(f"[DownloadManager] 音频任务已存在，跳过添加: {title}")
                            else:
                                print(f"[DownloadManager] 未找到音频链接，跳过音频下载: {title}")
                        else:
                            print(f"[DownloadManager] 获取音频信息失败: {audio_result.get('message')}")
                    except Exception as e:
                        print(f"[DownloadManager] 获取音频信息失败: {e}")

                return {'success': True, 'message': '已添加到 Aria2 下载队列', 'download_id': task.id}
            else:
                task.status = 'failed'
                task.error = result['message']
                db.session.commit()
                return {'success': False, 'message': result['message']}
    
    def _add_to_aria2(self, task, uid=None):
        """将任务添加到 Aria2

        Args:
            task: 下载任务对象
            uid: 博主UID，用于自动分文件夹
        """
        # 获取下载目录（支持按博主分文件夹）
        download_dir = self._get_download_dir(uid)

        # 构建 headers
        headers = {
            'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
            'Referer': 'https://www.bilibili.com/',
        }

        # 获取 Aria2 选项
        options = self._get_aria2_options()

        # 添加下载任务（使用自定义下载目录）
        result = self._aria2_manager.add_download(
            url=task.url,
            filename=task.filename,
            cookies=task.cookies,
            headers=headers,
            options={**options, 'dir': download_dir}
        )

        return result
    
    def _monitor_downloads(self):
        """监控 Aria2 下载状态的线程"""
        last_db_update = {}  # 记录上次数据库更新时间
        db_update_interval = 5  # 数据库更新间隔（秒）

        while self._running:
            try:
                if self._app and self._aria2_manager:
                    with self._app.app_context():
                        # 获取所有正在下载的任务
                        downloading_tasks = DownloadTask.query.filter(
                            DownloadTask.status.in_(['downloading', 'pending'])
                        ).all()

                        tasks_to_commit = []  # 需要提交到数据库的任务
                        completed_bvids = set() # 本轮完成的任务BVID

                        for task in downloading_tasks:
                            if task.gid:
                                # 获取 Aria2 状态
                                status = self._aria2_manager.get_download_status(task.gid)

                                if status['success']:
                                    aria2_status = status['status']
                                    current_time = time.time()

                                    # 更新内存缓存
                                    self._progress_cache[task.bvid] = {
                                        'progress_percent': status.get('progress_percent', 0),
                                        'downloaded_size': status.get('downloaded_size', 0),
                                        'total_size': status.get('total_size', 0),
                                        'speed': status.get('speed', 0),
                                        'last_update': current_time
                                    }

                                    # 状态变化时立即更新数据库
                                    status_changed = (task.status != aria2_status and aria2_status != 'active')

                                    # 定期更新数据库（每5秒）
                                    last_update = last_db_update.get(task.bvid, 0)
                                    should_update_db = (current_time - last_update >= db_update_interval)

                                    # 更新任务状态
                                    if aria2_status == 'complete':
                                        task.status = 'completed'
                                        task.progress_percent = 100
                                        task.downloaded_size = status['total_size']
                                        task.total_size = status['total_size']
                                        task.speed = 0
                                        
                                        # 实时更新真实文件名到数据库
                                        real_filename = status.get('filename')
                                        if (real_filename and 
                                            real_filename != 'Unknown' and 
                                            len(real_filename) > 4 and 
                                            '.' in real_filename):
                                            
                                            if task.filename != real_filename:
                                                task.filename = real_filename
                                                print(f"[DownloadManager] 下载完成，更新文件名到数据库: {task.bvid} -> {real_filename}")
                                        
                                        # 获取下载目录
                                        uid = self._get_blogger_uid_from_history(task.bvid)
                                        download_dir = self._get_download_dir(uid)

                                        # 使用数据库中的文件名
                                        final_path = os.path.join(download_dir, task.filename or f"{task.bvid}.m4s")

                                        # 添加到历史记录
                                        self._add_to_history(task, final_path, uid)

                                        # 广播下载完成
                                        self._broadcast_progress(task.bvid, {
                                            'type': task.type,
                                            'status': 'completed',
                                            'progress_percent': 100,
                                            'downloaded_size': status['total_size'],
                                            'total_size': status['total_size'],
                                            'speed': 0
                                        })

                                        # 检查并尝试合并音视频
                                        # 注意：只有成对的视频和音频（即服务器自动下载的）才会被合并
                                        # 单独手动下载的视频或音频（通常没有成对的另一半）不会触发合并
                                        print(f"[DownloadManager] 任务完成，加入合并检查队列 - BVID: {task.bvid}, 类型: {task.type}")
                                        completed_bvids.add(task.bvid)

                                        tasks_to_commit.append(task)
                                        last_db_update[task.bvid] = current_time

                                    elif aria2_status == 'error':
                                        task.status = 'failed'
                                        task.error = status.get('error_message', 'Unknown error')
                                        task.speed = 0

                                        # 广播下载失败
                                        self._broadcast_progress(task.bvid, {
                                            'type': task.type,
                                            'status': 'failed',
                                            'error': status.get('error_message', 'Unknown error')
                                        })

                                        tasks_to_commit.append(task)
                                        last_db_update[task.bvid] = current_time

                                    elif aria2_status == 'active':
                                        # 更新真实文件名（下载开始时就尝试捕获）
                                        real_filename = status.get('filename')
                                        if (real_filename and 
                                            real_filename != 'Unknown' and 
                                            len(real_filename) > 4 and 
                                            '.' in real_filename):
                                            
                                            if task.filename != real_filename:
                                                task.filename = real_filename
                                                status_changed = True
                                                print(f"[DownloadManager] 下载中，更新文件名到数据库: {task.bvid} -> {real_filename}")
                                        # 只在需要时更新数据库
                                        if status_changed or should_update_db or task.status != 'downloading':
                                            task.status = 'downloading'
                                            task.progress_percent = status['progress_percent']
                                            task.downloaded_size = status['downloaded_size']
                                            task.total_size = status['total_size']
                                            task.speed = status['speed']
                                            tasks_to_commit.append(task)
                                            last_db_update[task.bvid] = current_time

                                        # 广播下载进度（每次都广播，但不写入数据库）
                                        self._broadcast_progress(task.bvid, {
                                            'type': task.type,
                                            'status': 'downloading',
                                            'progress_percent': status['progress_percent'],
                                            'downloaded_size': status['downloaded_size'],
                                            'total_size': status['total_size'],
                                            'speed': status['speed']
                                        })

                                    elif aria2_status == 'waiting':
                                        if task.status != 'pending':
                                            task.status = 'pending'
                                            tasks_to_commit.append(task)

                                        # 广播等待状态
                                        self._broadcast_progress(task.bvid, {
                                            'type': task.type,
                                            'status': 'pending'
                                        })

                                    elif aria2_status == 'paused':
                                        if task.status != 'paused':
                                            task.status = 'paused'
                                            tasks_to_commit.append(task)

                                        # 广播暂停状态
                                        self._broadcast_progress(task.bvid, {
                                            'type': task.type,
                                            'status': 'paused'
                                        })

                        # 批量提交数据库更新
                        if tasks_to_commit:
                            try:
                                db.session.commit()
                                # 提交成功后，检查是否需要合并
                                for bvid in completed_bvids:
                                    self._check_and_trigger_merge(bvid)
                            except Exception as e:
                                print(f"[DownloadManager] 数据库提交失败: {e}")
                                db.session.rollback()

            except Exception as e:
                print(f"监控下载状态时出错: {e}")

            # 每 2 秒检查一次（如果没有任务则延长等待）
            if not downloading_tasks:
                time.sleep(5)
            else:
                time.sleep(2)
    
    def _broadcast_progress(self, bvid, data):
        """广播下载进度"""
        try:
            from app import broadcast_download_progress as _broadcast
            _broadcast(bvid, data)
            
            # 如果是错误状态或完成状态，发送弹窗通知
            if data.get('status') in ['failed', 'completed', 'merged']:
                # 构造通知消息
                message = ""
                msg_type = "info"
                
                if data.get('status') == 'failed':
                    message = f"下载失败: {data.get('error', '未知错误')}"
                    msg_type = "error"
                elif data.get('status') == 'completed':
                    message = f"下载完成: {bvid}"
                    msg_type = "success"
                elif data.get('status') == 'merged':
                    message = f"合并完成: {bvid}"
                    msg_type = "success"
                    if not data.get('success', True):
                        message = f"合并失败: {data.get('message', '未知错误')}"
                        msg_type = "error"
                
                # 发送通知事件
                from app import socketio
                socketio.emit('notification', {
                    'message': message,
                    'type': msg_type,
                    'bvid': bvid
                })
        except:
            pass

    def _check_and_trigger_merge(self, bvid):
        """
        检查给定bvid的视频和音频任务是否都已完成，如果完成则触发合并。
        """
        with self._app.app_context():
            video_task = DownloadTask.query.filter_by(bvid=bvid, type='video', status='completed').first()
            audio_task = DownloadTask.query.filter_by(bvid=bvid, type='audio', status='completed').first()

            if video_task and audio_task:
                print(f"[DownloadManager] 视频和音频任务均已完成，准备合并: {bvid}")
                print(f"[DownloadManager] 视频任务状态: {video_task.status}, 文件名: {video_task.filename}")
                print(f"[DownloadManager] 音频任务状态: {audio_task.status}, 文件名: {audio_task.filename}")
                
                # 获取下载目录
                uid = self._get_blogger_uid_from_history(bvid)
                download_dir = self._get_download_dir(uid)
                
                # 直接从数据库任务记录获取文件名
                video_filename = video_task.filename or f"{bvid}.m4s"
                audio_filename = audio_task.filename or f"{bvid}.m4s"
                
                video_file_path = os.path.join(download_dir, video_filename)
                audio_file_path = os.path.join(download_dir, audio_filename)
                
                print(f"[DownloadManager] 准备合并的文件:")
                print(f"  视频: {video_file_path}")
                print(f"  音频: {audio_file_path}")
                
                # 为了保持接口兼容，我们可以将路径传递给 _try_merge_audio_video
                # 但我们需要修改 _try_merge_audio_video 来优先使用传入的路径
                self._try_merge_audio_video(video_file_path, video_task, audio_file_path)
            else:
                if video_task:
                    print(f"[DownloadManager] 视频任务已完成，但音频任务未完成或不存在: {bvid}")
                elif audio_task:
                    print(f"[DownloadManager] 音频任务已完成，但视频任务未完成或不存在: {bvid}")
                else:
                    print(f"[DownloadManager] 视频和音频任务都未完成或不存在: {bvid}")

    def _get_blogger_uid_from_history(self, bvid):
        """从历史记录中获取博主UID"""
        try:
            history = History.query.filter_by(bvid=bvid).first()
            return history.uid if history else None
        except:
            return None

    def _try_merge_audio_video(self, video_path, task, audio_path=None):
        """尝试合并音视频并清理源文件"""
        try:
            from services.video_processor import get_video_processor

            video_processor = get_video_processor()

            if not video_processor.is_available():
                print(f"[DownloadManager] ffmpeg 不可用，跳过合并")
                return

            # 获取下载目录
            uid = self._get_blogger_uid_from_history(task.bvid)
            download_dir = self._get_download_dir(uid)
            
            print(f"[DownloadManager] 开始合并检查 - BVID: {task.bvid}")
            print(f"[DownloadManager] 下载目录: {download_dir}")
            print(f"[DownloadManager] 输入视频路径: {video_path}")
            if audio_path:
                print(f"[DownloadManager] 输入音频路径: {audio_path}")

            # 检查视频文件是否存在
            if not os.path.exists(video_path):
                print(f"[DownloadManager] 视频文件不存在: {video_path}，跳过合并")
                return

            # 如果没有传入音频路径，尝试从数据库获取
            if not audio_path:
                audio_task = DownloadTask.query.filter_by(bvid=task.bvid, type='audio').first()
                if audio_task:
                    audio_filename = audio_task.filename or f"{task.bvid}.m4s"
                    audio_path = os.path.join(download_dir, audio_filename)
                else:
                    print(f"[DownloadManager] 未找到音频任务: {task.bvid}")
                    return
            
            # 检查音频文件是否存在
            if not os.path.exists(audio_path):
                print(f"[DownloadManager] 音频文件不存在: {audio_path}，跳过合并")
                
                # 尝试查找可能的 .1 文件 (如果监控线程还没来得及更新数据库)
                # 这种情况可能发生在任务刚完成但监控线程还没跑完一轮时
                base, ext = os.path.splitext(audio_path)
                alt_path = f"{base}.1{ext}"
                if os.path.exists(alt_path):
                    print(f"[DownloadManager] 发现备用音频文件: {alt_path}")
                    audio_path = alt_path
                else:
                    return

            print(f"[DownloadManager] 检测到需要合并音视频: {task.bvid}")
            print(f"[DownloadManager] 视频文件: {video_path}")
            print(f"[DownloadManager] 音频文件: {audio_path}")

            # 构建输出路径（最终合并文件）
            sanitized_title = sanitize_filename(task.title)
            output_filename = f"{sanitized_title}_{task.bvid}.mp4"
            output_path = os.path.join(download_dir, output_filename)

            # 执行合并
            def on_merge_complete(result):
                if result.get('success'):
                    final_output_path = result.get('output_path')
                    print(f"[DownloadManager] 音视频合并成功: {final_output_path}")

                    # 更新历史记录中的文件路径
                    self._update_history_file_path(task.bvid, final_output_path)
                else:
                    print(f"[DownloadManager] 音视频合并失败: {result.get('message')}")

                # 广播合并完成状态
                self._broadcast_progress(task.bvid, {
                    'type': 'video',
                    'status': 'merged',
                    'message': '音视频合并完成' if result.get('success') else '音视频合并失败',
                    'success': result.get('success'),
                    'output_path': output_path if result.get('success') else None
                })

            result = video_processor.merge_and_cleanup(video_path, audio_path, output_path, on_merge_complete)

            if result.get('success'):
                print(f"[DownloadManager] 已启动音视频合并任务")

        except Exception as e:
            print(f"[DownloadManager] 尝试合并音视频时出错: {e}")
    
    def _update_history_file_path(self, bvid, new_path):
        """更新历史记录中的文件路径"""
        try:
            history = History.query.filter_by(bvid=bvid).first()
            if history:
                history.file_path = new_path
                db.session.commit()
                print(f"[DownloadManager] 已更新历史记录文件路径: {new_path}")
        except Exception as e:
            print(f"[DownloadManager] 更新历史记录失败: {e}")

    def _add_to_history(self, task, file_path, uid):
        """添加到下载历史"""
        try:
            # 检查是否已存在
            existing = History.query.filter_by(bvid=task.bvid).first()
            if existing:
                return

            # 获取视频信息
            uid_to_save = uid
            pub_date = None
            try:
                result = get_bili_api().get_video_info(task.bvid, task.cookies)
                if result.get('success'):
                    owner = result.get('owner', {})
                    api_uid = str(owner.get('mid', ''))
                    if api_uid:
                        uid_to_save = api_uid
                    pub_date = datetime.fromtimestamp(result.get('created', 0)).strftime('%Y-%m-%d') if result.get('created') else None
            except Exception as e:
                print(f"[DownloadManager] 获取视频信息失败: {e}, 使用备用信息")

            history = History(
                uid=uid_to_save,
                bvid=task.bvid,
                title=task.title,
                pub_date=pub_date,
                file_path=file_path
            )
            db.session.add(history)
            db.session.commit()
            print(f"[DownloadManager] 已添加历史记录: {task.bvid} -> {uid_to_save}")
            
            # 尝试清理历史记录
            try:
                History.cleanup_history()
            except:
                pass
        except Exception as e:
            print(f"添加历史记录失败: {e}")

    def _get_uid_from_path(self, file_path):
        """从文件路径推断 UID"""
        try:
            # 路径格式通常是: {base_path}/{uid}/{filename}
            parts = file_path.split(os.sep)
            if len(parts) >= 2:
                # 倒数第二个目录可能是 UID
                potential_uid = parts[-2]
                # 检查是否是纯数字（UID 通常是数字）
                if potential_uid.isdigit():
                    return potential_uid
        except:
            pass
        return ''
    
    def get_progress(self):
        """获取所有下载进度"""
        with self._app.app_context():
            tasks = DownloadTask.query.all()
            downloads = {}
            for task in tasks:
                # 优先使用内存缓存的进度（如果是正在下载的任务）
                cache = self._progress_cache.get(task.bvid, {})
                if task.status == 'downloading' and cache:
                    downloads[task.bvid] = {
                        'filename': task.filename,
                        'status': task.status,
                        'progress_percent': cache.get('progress_percent', task.progress_percent),
                        'downloaded_size': cache.get('downloaded_size', task.downloaded_size),
                        'total_size': cache.get('total_size', task.total_size),
                        'speed': cache.get('speed', task.speed),
                        'gid': task.gid
                    }
                else:
                    downloads[task.bvid] = {
                        'filename': task.filename,
                        'status': task.status,
                        'progress_percent': task.progress_percent,
                        'downloaded_size': task.downloaded_size,
                        'total_size': task.total_size,
                        'speed': task.speed,
                        'gid': task.gid
                    }
            return downloads
    
    def retry_task(self, bvid, task_type='video'):
        """重试下载任务"""
        with self._app.app_context():
            task = DownloadTask.query.filter_by(bvid=bvid, type=task_type).first()
            if not task:
                return {'success': False, 'message': '未找到下载任务'}

            if task.status == 'downloading':
                return {'success': False, 'message': '该任务正在下载中'}

            task.status = 'pending'
            task.error = None
            task.progress_percent = 0
            task.downloaded_size = 0
            task.gid = None
            # 强制重置文件名为 BV号.m4s
            task.filename = f"{bvid}.m4s"
            db.session.commit()

            # 重新添加到 Aria2
            result = self._add_to_aria2(task)
            if result['success']:
                task.gid = result['gid']
                task.status = 'downloading'
                db.session.commit()
                return {'success': True, 'message': '已重新加入下载队列'}
            else:
                task.status = 'failed'
                task.error = result['message']
                db.session.commit()
                return {'success': False, 'message': result['message']}
    
    def retry_all_failed(self):
        """重试所有失败的任务"""
        with self._app.app_context():
            failed_tasks = DownloadTask.query.filter_by(status='failed').all()
            count = 0
            failed_count = 0
            
            for task in failed_tasks:
                task.status = 'pending'
                task.error = None
                task.progress_percent = 0
                task.downloaded_size = 0
                task.gid = None
                # 强制重置文件名为 BV号.m4s
                task.filename = f"{task.bvid}.m4s"
                db.session.commit()
                
                # 重新添加到 Aria2
                result = self._add_to_aria2(task)
                if result['success']:
                    task.gid = result['gid']
                    task.status = 'downloading'
                    db.session.commit()
                    count += 1
                else:
                    task.status = 'failed'
                    task.error = result['message']
                    db.session.commit()
                    failed_count += 1

            return {'success': True, 'message': f'已重试 {count} 个失败的下载任务，{failed_count} 个重试失败'}
    
    def remove_task(self, bvid, task_type='video'):
        """移除下载任务"""
        with self._app.app_context():
            task = DownloadTask.query.filter_by(bvid=bvid, type=task_type).first()
            if not task:
                return {'success': False, 'message': '未找到下载任务'}

            # 如果任务在 Aria2 中，先移除
            if task.gid and self._aria2_manager:
                try:
                    self._aria2_manager.remove_download(task.gid)
                except Exception as e:
                    print(f"[DownloadManager] Aria2任务移除失败 (可能已自动移除): {e}")
                    # 继续执行数据库删除

            db.session.delete(task)
            db.session.commit()

            return {'success': True, 'message': '已移除下载记录'}
    
    def get_status(self, uid=None):
        """获取下载状态统计

        Args:
            uid: 博主UID，为None则返回所有
        """
        with self._app.app_context():
            # 构建基础查询
            base_query = DownloadTask.query

            # 如果指定了UID，通过History表关联查询
            if uid:
                # 获取该博主的所有BVID
                bvid_list = [h.bvid for h in History.query.filter_by(uid=uid).all()]
                if bvid_list:
                    base_query = base_query.filter(DownloadTask.bvid.in_(bvid_list))
                else:
                    # 该博主没有历史记录，返回空
                    return {
                        'success': True,
                        'stats': {'pending': 0, 'downloading': 0, 'completed': 0, 'failed': 0},
                        'statuses': {},
                        'aria2_connected': self._aria2_manager.is_available() if self._aria2_manager else False
                    }

            stats = {
                'pending': base_query.filter_by(status='pending').count(),
                'downloading': base_query.filter_by(status='downloading').count(),
                'completed': base_query.filter_by(status='completed').count(),
                'failed': base_query.filter_by(status='failed').count()
            }

            statuses = {}
            tasks = base_query.all()
            for task in tasks:
                # 使用 bvid_type 作为键，避免同一BV号的视频和音频冲突
                key = f"{task.bvid}_{task.type}"
                statuses[key] = {
                    'bvid': task.bvid,
                    'type': task.type,
                    'title': task.title,
                    'status': task.status,
                    'progress_percent': task.progress_percent,
                    'downloaded_size': task.downloaded_size,
                    'total_size': task.total_size,
                    'speed': task.speed,
                    'error': task.error,
                    'updated_at': task.updated_at.strftime('%Y-%m-%d %H:%M:%S') if task.updated_at else None
                }

            # 检查 Aria2 连接状态
            aria2_connected = False
            if self._aria2_manager:
                aria2_connected = self._aria2_manager.is_available()

            return {
                'success': True,
                'stats': stats,
                'statuses': statuses,
                'aria2_connected': aria2_connected
            }

    def get_status_by_blogger(self):
        """按博主分组获取下载状态"""
        with self._app.app_context():
            result = {}

            # 获取所有历史记录中的博主
            bloggers = db.session.query(History.uid).distinct().all()

            for (uid,) in bloggers:
                if not uid:
                    continue

                # 获取该博主的所有BVID
                bvid_list = [h.bvid for h in History.query.filter_by(uid=uid).all()]

                if bvid_list:
                    tasks = DownloadTask.query.filter(DownloadTask.bvid.in_(bvid_list)).all()

                    result[uid] = {
                        'pending': sum(1 for t in tasks if t.status == 'pending'),
                        'downloading': sum(1 for t in tasks if t.status == 'downloading'),
                        'completed': sum(1 for t in tasks if t.status == 'completed'),
                        'failed': sum(1 for t in tasks if t.status == 'failed'),
                        'total': len(tasks)
                    }

            # 检查 Aria2 连接状态
            aria2_connected = False
            if self._aria2_manager:
                aria2_connected = self._aria2_manager.is_available()

            return {
                'success': True,
                'bloggers': result,
                'aria2_connected': aria2_connected
            }
    
    def stop(self):
        """停止监控线程"""
        self._running = False
        if self._monitor_thread:
            self._monitor_thread.join(timeout=5)

# 全局下载管理器实例
_download_manager = None

def get_download_manager() -> DownloadManager:
    """获取全局下载管理器实例"""
    global _download_manager
    if _download_manager is None:
        _download_manager = DownloadManager()
    return _download_manager

@download_bp.route('/add', methods=['POST'])
def add_download():
    """添加下载任务"""
    data = request.get_json()

    bvid = data.get('bvid', '').strip()
    title = data.get('title', '').strip()
    url = data.get('url', '').strip()
    cookies = data.get('cookies', '').strip()
    # 从数据库读取视频质量设置
    settings = Setting.get_all_settings()
    default_quality = settings.get('query').get('video_quality')
    quality = data.get('quality', default_quality)
    task_type = data.get('type', 'video')
    
    if not bvid or not url:
        return jsonify({'success': False, 'message': '请提供BV号和下载链接'})
    
    result = get_download_manager().add_task(bvid, title, url, cookies, quality, task_type)
    return jsonify(result)

@download_bp.route('/progress', methods=['GET'])
def get_progress():
    """获取下载进度"""
    downloads = get_download_manager().get_progress()
    return jsonify({'success': True, 'downloads': downloads})

@download_bp.route('/retry', methods=['POST'])
def retry_download():
    """重试下载"""
    data = request.get_json()
    bvid = data.get('bvid', '').strip()
    task_type = data.get('type', 'video').strip()

    if not bvid:
        return jsonify({'success': False, 'message': '请提供BV号'})

    result = get_download_manager().retry_task(bvid, task_type)
    return jsonify(result)

@download_bp.route('/retry_all', methods=['POST'])
def retry_all():
    """重试所有失败的任务"""
    result = get_download_manager().retry_all_failed()
    return jsonify(result)

@download_bp.route('/remove', methods=['POST'])
def remove_download():
    """移除下载记录"""
    data = request.get_json()
    bvid = data.get('bvid', '').strip()
    task_type = data.get('type', 'video').strip()

    if not bvid:
        return jsonify({'success': False, 'message': '请提供BV号'})

    result = get_download_manager().remove_task(bvid, task_type)
    return jsonify(result)

@download_bp.route('/status', methods=['GET'])
def get_status():
    """获取下载状态统计"""
    uid = request.args.get('uid', '').strip()
    result = get_download_manager().get_status(uid if uid else None)
    return jsonify(result)

@download_bp.route('/status_by_blogger', methods=['GET'])
def get_status_by_blogger():
    """按博主分组获取下载状态"""
    result = get_download_manager().get_status_by_blogger()
    return jsonify(result)

@download_bp.route('/proxy', methods=['GET'])
def download_proxy():
    """下载代理（浏览器下载用）"""
    url = request.args.get('url')
    filename = request.args.get('filename', 'download')
    cookies_str = request.args.get('cookies', '')

    if not url:
        return jsonify({'success': False, 'message': '请提供下载链接'}), 400

    try:
        # 解析cookies
        cookies = {}
        if cookies_str:
            for item in cookies_str.split(';'):
                item = item.strip()
                if '=' in item:
                    k, v = item.split('=', 1)
                    cookies[k.strip()] = v.strip()

        # 设置请求头 - 移除 Range 头，与 demo.py 保持一致
        headers = {
            'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
            'Referer': 'https://www.bilibili.com/',
            'Accept': '*/*',
        }

        # 发送请求，流式下载
        response = requests.get(url, headers=headers, cookies=cookies, stream=True, timeout=60)
        response.raise_for_status()
        
        # 获取文件大小和类型
        content_length = response.headers.get('Content-Length')
        content_type = response.headers.get('Content-Type', 'application/octet-stream')
        
        # 构建响应
        def generate():
            for chunk in response.iter_content(chunk_size=1024*1024):
                if chunk:
                    yield chunk
        
        # 设置响应头
        from flask import Response
        resp = Response(generate(), mimetype=content_type)
        # RFC 5987 编码处理中文文件名
        try:
            # 尝试使用 ASCII 文件名
            filename.encode('ascii')
            resp.headers['Content-Disposition'] = f'attachment; filename="{filename}"'
        except UnicodeEncodeError:
            # 非 ASCII 字符使用 RFC 5987 编码
            encoded_filename = quote(filename, safe='')
            resp.headers['Content-Disposition'] = f"attachment; filename*=UTF-8''{encoded_filename}"
        resp.headers['Cache-Control'] = 'no-cache'
        
        if content_length:
            resp.headers['Content-Length'] = content_length
        
        return resp
        
    except Exception as e:
        # Sanitize error message to avoid leaking internal details
        error_msg = str(e)
        # Log the full error for debugging
        print(f"[DownloadProxy] Error: {error_msg}")
        return jsonify({'success': False, 'message': '下载失败: 服务器内部错误'}), 500
