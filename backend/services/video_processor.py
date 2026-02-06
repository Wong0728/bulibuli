"""
视频处理服务 - 使用 ffmpeg 合并音视频
"""
import os
import subprocess
import threading
import time
from typing import Optional, Dict, Any, Callable
from datetime import datetime


def get_ffmpeg_path() -> str:
    """获取 ffmpeg.exe 的路径"""
    import sys
    
    # 检查是否是打包环境
    if hasattr(sys, '_MEIPASS'):
        # PyInstaller 打包后的环境
        # 资源文件通常位于 _MEIPASS/_internal/resources 下
        # 或者直接在 _MEIPASS/resources 下，取决于打包配置
        # 这里尝试直接从 _MEIPASS 获取，因为通常资源会解压到这里
        base_path = sys._MEIPASS
    else:
        # 开发环境
        backend_dir = os.path.dirname(os.path.dirname(__file__))
        base_path = os.path.dirname(backend_dir)
        
    ffmpeg_path = os.path.join(base_path, 'resources', 'ffmpeg.exe')
    
    # 如果没找到，打印调试信息（虽然在无控制台模式下看不到）
    if not os.path.exists(ffmpeg_path):
        # 尝试备用路径：对于单目录模式，可能是 _internal/resources
        alt_path = os.path.join(os.path.dirname(sys.executable), '_internal', 'resources', 'ffmpeg.exe')
        if os.path.exists(alt_path):
            return alt_path
            
    return ffmpeg_path


class VideoProcessor:
    """视频处理器 - 合并音视频等操作"""

    def __init__(self):
        self.ffmpeg_path = get_ffmpeg_path()
        self._lock = threading.Lock()
        self._tasks = {}  # task_id -> task_info
        self._callbacks = {}  # task_id -> callback

    def is_available(self) -> bool:
        """检查 ffmpeg 是否可用"""
        if not os.path.exists(self.ffmpeg_path):
            return False
        try:
            result = subprocess.run(
                [self.ffmpeg_path, '-version'],
                capture_output=True,
                timeout=5
            )
            return result.returncode == 0
        except:
            return False

    def merge_audio_video(self, video_path: str, audio_path: str, output_path: str,
                          callback: Optional[Callable] = None) -> Dict[str, Any]:
        """
        合并视频和音频文件

        Args:
            video_path: 视频文件路径
            audio_path: 音频文件路径
            output_path: 输出文件路径
            callback: 完成后的回调函数

        Returns:
            包含任务ID和状态的结果
        """
        if not self.is_available():
            return {
                'success': False,
                'message': f'ffmpeg 不可用，请确保文件存在: {self.ffmpeg_path}'
            }

        if not os.path.exists(video_path):
            return {
                'success': False,
                'message': f'视频文件不存在: {video_path}'
            }

        if not os.path.exists(audio_path):
            return {
                'success': False,
                'message': f'音频文件不存在: {audio_path}'
            }

        # 确保输出目录存在
        output_dir = os.path.dirname(output_path)
        if output_dir:
            os.makedirs(output_dir, exist_ok=True)

        # 生成任务ID
        task_id = f"merge_{int(time.time() * 1000)}"

        # 构建 ffmpeg 命令
        # 使用 copy 编码器直接复制流，不重新编码，速度快且无损
        cmd = [
            self.ffmpeg_path,
            '-i', video_path,  # 输入视频
            '-i', audio_path,  # 输入音频
            '-c', 'copy',      # 直接复制流，不重新编码
            '-y',              # 覆盖输出文件
            output_path
        ]

        try:
            # 启动合并进程
            process = subprocess.Popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                creationflags=subprocess.CREATE_NO_WINDOW  # Windows 下不显示控制台窗口
            )

            with self._lock:
                self._tasks[task_id] = {
                    'process': process,
                    'video_path': video_path,
                    'audio_path': audio_path,
                    'output_path': output_path,
                    'status': 'running',
                    'start_time': datetime.now(),
                    'callback': callback
                }

            # 启动监控线程
            monitor_thread = threading.Thread(
                target=self._monitor_merge_task,
                args=(task_id,),
                daemon=True
            )
            monitor_thread.start()

            return {
                'success': True,
                'task_id': task_id,
                'message': '合并任务已启动',
                'output_path': output_path
            }

        except Exception as e:
            return {
                'success': False,
                'message': f'启动合并任务失败: {str(e)}'
            }

    def _monitor_merge_task(self, task_id: str):
        """监控合并任务"""
        with self._lock:
            task = self._tasks.get(task_id)

        if not task:
            return

        process = task['process']

        # 等待进程完成
        stdout, stderr = process.communicate()

        with self._lock:
            task = self._tasks.get(task_id)
            if not task:
                return

            if process.returncode == 0:
                task['status'] = 'completed'

                result = {
                    'success': True,
                    'task_id': task_id,
                    'output_path': task['output_path'],
                    'message': '合并完成'
                }
            else:
                task['status'] = 'failed'
                error_msg = stderr[-500:] if stderr else 'Unknown error'  # 取最后500字符
                result = {
                    'success': False,
                    'task_id': task_id,
                    'message': f'合并失败: {error_msg}'
                }

            # 执行回调
            callback = task.get('callback')
            if callback:
                try:
                    callback(result)
                except Exception as e:
                    print(f"回调执行失败: {e}")

    def _cleanup_source_files(self, video_path: str, audio_path: str):
        """清理源文件"""
        try:
            if os.path.exists(video_path):
                os.remove(video_path)
                print(f"[VideoProcessor] 已删除视频源文件: {video_path}")
            if os.path.exists(audio_path):
                os.remove(audio_path)
                print(f"[VideoProcessor] 已删除音频源文件: {audio_path}")
        except Exception as e:
            print(f"[VideoProcessor] 清理源文件失败: {e}")

    def merge_and_cleanup(self, video_path: str, audio_path: str, output_path: str,
                          callback: Optional[Callable] = None) -> Dict[str, Any]:
        """
        合并音视频并清理源文件
        
        Args:
            video_path: 视频文件路径
            audio_path: 音频文件路径
            output_path: 输出文件路径
            callback: 完成后的回调函数
            
        Returns:
            包含任务ID和状态的结果
        """
        def on_merge_complete(result):
            if result.get('success'):
                print(f"[VideoProcessor] 音视频合并完成，正在清理源文件...")
                self._cleanup_source_files(video_path, audio_path)
            
            # 调用用户回调
            if callback:
                try:
                    callback(result)
                except Exception as e:
                    print(f"[VideoProcessor] 回调执行失败: {e}")
        
        # 执行合并
        return self.merge_audio_video(video_path, audio_path, output_path, on_merge_complete)

    def get_task_status(self, task_id: str) -> Dict[str, Any]:
        """获取任务状态"""
        with self._lock:
            task = self._tasks.get(task_id)

        if not task:
            return {
                'success': False,
                'message': '未找到任务'
            }

        return {
            'success': True,
            'task_id': task_id,
            'status': task['status'],
            'output_path': task.get('output_path'),
            'start_time': task.get('start_time').isoformat() if task.get('start_time') else None
        }

    def check_needs_merge(self, file_path: str) -> bool:
        """
        检查文件是否需要合并（检查是否存在对应的音频文件）

        Args:
            file_path: 视频文件路径

        Returns:
            是否需要合并
        """
        if not file_path or not os.path.exists(file_path):
            return False

        # 检查是否存在对应的音频文件
        base_name = os.path.splitext(file_path)[0]
        audio_extensions = ['.m4a', '.aac', '.mp3', '.wav', '.m4s']

        for ext in audio_extensions:
            audio_path = base_name + ext
            if os.path.exists(audio_path):
                return True

        return False

    def auto_merge_if_needed(self, video_path: str, callback: Optional[Callable] = None) -> Dict[str, Any]:
        """
        如果需要，自动合并音视频

        Args:
            video_path: 视频文件路径
            callback: 完成后的回调函数

        Returns:
            合并结果
        """
        if not self.check_needs_merge(video_path):
            return {
                'success': True,
                'message': '无需合并',
                'merged': False
            }

        base_name = os.path.splitext(video_path)[0]
        audio_extensions = ['.m4a', '.aac', '.mp3', '.wav', '.m4s']
        audio_path = None

        for ext in audio_extensions:
            potential_path = base_name + ext
            if os.path.exists(potential_path):
                audio_path = potential_path
                break

        if not audio_path:
            return {
                'success': False,
                'message': '未找到对应的音频文件'
            }

        # 构建输出路径
        output_path = base_name + '_merged.mp4'

        return self.merge_audio_video(video_path, audio_path, output_path, callback)


# 全局视频处理器实例
_video_processor = None


def get_video_processor() -> VideoProcessor:
    """获取全局视频处理器实例"""
    global _video_processor
    if _video_processor is None:
        _video_processor = VideoProcessor()
    return _video_processor
