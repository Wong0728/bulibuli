"""
Aria2 下载服务模块
支持两种方式：
1. RPC 方式（推荐）- 连接到 Aria2 RPC 服务
2. 本地 aria2c.exe 子进程方式（备选）

推荐配置：
- 启动 aria2c.exe --enable-rpc --rpc-listen-port=6800
- Python 通过 JSON-RPC 连接获取精确进度
"""
from config import Config
import os
import subprocess
import threading
import time
import json
import requests
from typing import Optional, Dict, Any, List
from datetime import datetime
from urllib.parse import quote


# Aria2 配置 - 使用外部RPC服务，不再内嵌 aria2c.exe
def get_aria2c_path() -> str:
    """获取 aria2c.exe 的路径（已弃用，使用外部RPC服务）"""
    # 不再使用内嵌的 aria2c.exe
    # 用户需要自行安装并配置 Aria2 RPC 服务
    return None


class Aria2RPCServer:
    """Aria2 RPC 服务器管理器 - 仅用于检查外部服务状态，不再启动本地服务"""

    def __init__(self, port: int = 6800, download_dir: str = None):
        self.port = port
        self.download_dir = download_dir or Config.DOWNLOAD_DIR
        self._lock = threading.Lock()

    def is_running(self) -> bool:
        """检查 RPC 服务是否正在运行"""
        try:
            response = requests.post(
                f'http://localhost:{self.port}/jsonrpc',
                json={
                    'jsonrpc': '2.0',
                    'id': 'test',
                    'method': 'aria2.getVersion',
                    'params': []
                },
                timeout=2
            )
            return response.status_code == 200
        except:
            return False

    def start(self) -> bool:
        """检查外部 Aria2 RPC 服务是否可用（不再启动本地服务）"""
        if self.is_running():
            print(f"[Aria2RPCServer] 外部 Aria2 RPC 服务已在端口 {self.port} 运行")
            return True
        else:
            print(f"[Aria2RPCServer] 警告: 无法连接到外部 Aria2 RPC 服务 (端口 {self.port})")
            print(f"[Aria2RPCServer] 请确保 Aria2 已启动并启用了 RPC: aria2c --enable-rpc --rpc-listen-port={self.port}")
            return False

    def stop(self):
        """停止 RPC 服务（不再管理外部服务）"""
        pass


# 全局 RPC 服务器实例
_rpc_server = None


def get_rpc_server(port: int = 6800, download_dir: str = None) -> Aria2RPCServer:
    """获取全局 RPC 服务器实例（仅用于检查外部服务）"""
    global _rpc_server
    if _rpc_server is None or _rpc_server.port != port:
        _rpc_server = Aria2RPCServer(port, download_dir)
    return _rpc_server


class LocalAria2Downloader:
    """本地下载器 - 使用 requests 直接下载（当 Aria2 RPC 不可用时使用）"""
    
    def __init__(self, download_dir: str):
        """
        初始化本地下载器
        
        Args:
            download_dir: 下载目录
        """
        self.download_dir = download_dir
        self._active_downloads = {}  # gid -> download_info
        self._lock = threading.Lock()
        os.makedirs(download_dir, exist_ok=True)
    
    def is_available(self) -> bool:
        """本地下载器始终可用"""
        return True
    
    def add_download(self, url: str, filename: str, cookies: str = None,
                     headers: Dict[str, str] = None, options: Dict[str, Any] = None) -> Dict[str, Any]:
        """
        添加下载任务（使用 requests 直接下载）
        
        Args:
            url: 下载链接
            filename: 文件名
            cookies: Cookie 字符串
            headers: 请求头
            options: 额外选项
            
        Returns:
            包含 gid 和状态的结果
        """
        # 构建文件完整路径
        download_dir = options.get('dir', self.download_dir) if options else self.download_dir
        file_path = os.path.join(download_dir, filename)
        
        # 确保下载目录存在
        os.makedirs(download_dir, exist_ok=True)
        
        # 生成唯一的 GID
        gid = f"local_{int(time.time() * 1000)}_{filename[:20]}"
        
        # 准备请求头
        request_headers = {
            'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
            'Referer': 'https://www.bilibili.com/',
        }
        if headers:
            request_headers.update(headers)
        
        # 准备 cookies
        request_cookies = {}
        if cookies:
            for item in cookies.split(';'):
                item = item.strip()
                if '=' in item:
                    k, v = item.split('=', 1)
                    request_cookies[k.strip()] = v.strip()
        
        # 存储下载信息
        with self._lock:
            self._active_downloads[gid] = {
                'filename': filename,
                'file_path': file_path,
                'url': url,
                'start_time': datetime.now(),
                'bvid': options.get('bvid') if options else None,
                'status': 'active',
                'downloaded_size': 0,
                'total_size': 0,
                'speed': 0,
                'stop_event': threading.Event()
            }
        
        # 启动后台下载线程
        download_thread = threading.Thread(
            target=self._download_worker,
            args=(gid, url, file_path, request_headers, request_cookies),
            daemon=True
        )
        download_thread.start()
        
        return {
            'success': True,
            'gid': gid,
            'message': '下载任务已启动（使用本地下载）',
            'filename': filename,
            'download_dir': download_dir
        }
    
    def _download_worker(self, gid: str, url: str, file_path: str, headers: Dict[str, str], cookies: Dict[str, str]):
        """后台下载工作线程"""
        try:
            with self._lock:
                download_info = self._active_downloads.get(gid)
                if not download_info:
                    return
                download_info['status'] = 'active'
            
            # 发送请求
            response = requests.get(url, headers=headers, cookies=cookies, stream=True, timeout=60)
            response.raise_for_status()
            
            # 获取文件大小
            total_size = int(response.headers.get('Content-Length', 0))
            
            with self._lock:
                download_info = self._active_downloads.get(gid)
                if download_info:
                    download_info['total_size'] = total_size
            
            # 下载文件
            downloaded = 0
            last_update_time = time.time()
            last_downloaded = 0
            
            with open(file_path, 'wb') as f:
                for chunk in response.iter_content(chunk_size=8192):
                    with self._lock:
                        if download_info and download_info.get('stop_event') and download_info['stop_event'].is_set():
                            download_info['status'] = 'paused'
                            return
                    
                    if chunk:
                        f.write(chunk)
                        downloaded += len(chunk)
                        
                        # 计算速度
                        current_time = time.time()
                        time_diff = current_time - last_update_time
                        if time_diff >= 1:  # 每秒更新一次
                            speed = (downloaded - last_downloaded) / time_diff
                            
                            with self._lock:
                                download_info = self._active_downloads.get(gid)
                                if download_info:
                                    download_info['downloaded_size'] = downloaded
                                    download_info['speed'] = int(speed)
                            
                            last_update_time = current_time
                            last_downloaded = downloaded
            
            # 下载完成
            with self._lock:
                download_info = self._active_downloads.get(gid)
                if download_info:
                    download_info['status'] = 'complete'
                    download_info['downloaded_size'] = downloaded
                    download_info['total_size'] = downloaded
                    download_info['speed'] = 0
                    
        except Exception as e:
            with self._lock:
                download_info = self._active_downloads.get(gid)
                if download_info:
                    download_info['status'] = 'error'
                    download_info['error_message'] = str(e)
    
    def get_download_status(self, gid: str) -> Dict[str, Any]:
        """
        获取下载任务状态
        
        Args:
            gid: 任务 GID
            
        Returns:
            任务状态信息
        """
        with self._lock:
            download_info = self._active_downloads.get(gid)
        
        if not download_info:
            return {
                'success': False,
                'message': '未找到下载任务'
            }
        
        file_path = download_info['file_path']
        status = download_info.get('status', 'active')
        downloaded_size = download_info.get('downloaded_size', 0)
        total_size = download_info.get('total_size', 0)
        speed = download_info.get('speed', 0)
        
        # 计算进度百分比
        progress_percent = 0
        if total_size > 0:
            progress_percent = int((downloaded_size / total_size) * 100)
        elif status == 'complete':
            progress_percent = 100
        
        # 如果状态是完成或错误，清理记录
        if status in ['complete', 'error']:
            # 延迟清理，让前端有机会获取最终状态
            pass
        
        return {
            'success': True,
            'gid': gid,
            'status': status,
            'total_size': total_size,
            'downloaded_size': downloaded_size,
            'progress_percent': progress_percent,
            'speed': speed,
            'error_message': download_info.get('error_message', ''),
            'filename': download_info['filename']
        }
    
    def get_all_downloads(self) -> List[Dict[str, Any]]:
        """获取所有下载任务"""
        with self._lock:
            gids = list(self._active_downloads.keys())
        
        result = []
        for gid in gids:
            status = self.get_download_status(gid)
            if status['success']:
                result.append({
                    'gid': gid,
                    'status': status['status'],
                    'filename': status['filename'],
                    'total_size': status['total_size'],
                    'downloaded_size': status['downloaded_size'],
                    'progress_percent': status['progress_percent'],
                    'speed': status['speed']
                })
        
        return result
    
    def remove_download(self, gid: str) -> bool:
        """移除下载任务"""
        with self._lock:
            download_info = self._active_downloads.get(gid)
            if download_info:
                # 设置停止事件
                stop_event = download_info.get('stop_event')
                if stop_event:
                    stop_event.set()
                download_info['status'] = 'paused'
                return True
        return False


class Aria2RPC:
    """Aria2 RPC 客户端（用于连接到外部 Aria2 服务）"""
    
    def __init__(self, host: str = 'localhost', port: int = 6800, secret: str = ''):
        self.host = host
        self.port = port
        self.secret = secret
        self.rpc_url = f'http://{host}:{port}/jsonrpc'
        self._request_id = 0
    
    def _get_request_id(self) -> str:
        self._request_id += 1
        return f'pyaria2_{self._request_id}'
    
    def _call(self, method: str, params: List[Any] = None) -> Dict[str, Any]:
        if params is None:
            params = []
        
        if self.secret:
            params.insert(0, f'token:{self.secret}')
        
        payload = {
            'jsonrpc': '2.0',
            'id': self._get_request_id(),
            'method': method,
            'params': params
        }
        
        try:
            response = requests.post(
                self.rpc_url,
                data=json.dumps(payload),
                headers={'Content-Type': 'application/json'},
                timeout=10
            )
            response.raise_for_status()
            result = response.json()
            
            if 'error' in result:
                raise Exception(f"Aria2 RPC Error: {result['error']}")
            
            return result.get('result', {})
        except requests.exceptions.ConnectionError:
            raise Exception(f"无法连接到 Aria2 RPC 服务 ({self.rpc_url})")
        except Exception as e:
            raise Exception(f"Aria2 RPC 调用失败: {str(e)}")
    
    def add_uri(self, uris: List[str], options: Dict[str, Any] = None) -> str:
        if options is None:
            options = {}
        return self._call('aria2.addUri', [uris, options])
    
    def tell_status(self, gid: str, keys: List[str] = None) -> Dict[str, Any]:
        params = [gid]
        if keys:
            params.append(keys)
        return self._call('aria2.tellStatus', params)
    
    def is_connected(self) -> bool:
        try:
            self._call('aria2.getVersion')
            return True
        except:
            return False


class Aria2DownloadManager:
    """Aria2 下载管理器 - 优先使用 RPC 方式，RPC 不可用时使用本地下载"""
    
    def __init__(self, download_dir: str, use_rpc: bool = True,
                 host: str = 'localhost', port: int = 6800, secret: str = ''):
        """
        初始化下载管理器
        
        Args:
            download_dir: 下载目录
            use_rpc: 是否使用 RPC 方式（默认 True）
            host: RPC 主机地址
            port: RPC 端口
            secret: RPC 密钥
        """
        self.download_dir = download_dir
        self.use_rpc = use_rpc
        self.rpc_host = host
        self.rpc_port = port
        self.rpc_secret = secret
        
        # 初始化 RPC 客户端
        self.rpc_client = Aria2RPC(host, port, secret)
        
        # 初始化本地下载器（作为备用）
        self.local_downloader = LocalAria2Downloader(download_dir)
        
        os.makedirs(download_dir, exist_ok=True)
    
    def is_available(self) -> bool:
        """检查下载器是否可用（RPC 或本地下载器至少一个可用）"""
        # 优先检查 RPC
        if self.rpc_client.is_connected():
            return True
        # RPC 不可用时，检查本地下载器
        return self.local_downloader.is_available()
    
    def is_rpc_available(self) -> bool:
        """检查 RPC 是否可用"""
        return self.rpc_client.is_connected()
    
    def add_download(self, url: str, filename: str, cookies: str = None,
                     headers: Dict[str, str] = None, options: Dict[str, Any] = None) -> Dict[str, Any]:
        """添加下载任务（优先使用 RPC，RPC 不可用时使用本地下载）"""
        # 优先尝试 RPC 方式
        if self.use_rpc and self.rpc_client and self.rpc_client.is_connected():
            # RPC 方式
            aria2_options = {
                'dir': self.download_dir,
                'out': filename,
                'split': '16',
                'max-connection-per-server': '16',
                'min-split-size': '10M',
                'max-tries': '5',
                'retry-wait': '5',
                'user-agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
                'referer': 'https://www.bilibili.com/',
            }
            
            if headers:
                header_list = [f"{k}: {v}" for k, v in headers.items()]
                aria2_options['header'] = header_list
            
            if cookies:
                aria2_options['header'] = aria2_options.get('header', []) + [f"Cookie: {cookies}"]
            
            if options:
                aria2_options.update(options)
            
            # 严格适配 Demo 逻辑：强制指定 out 参数为 BV 号文件名
            # 确保文件名不被覆盖，且 Aria2 严格按照指定文件名保存
            aria2_options['out'] = filename
            # 使用 ANSI 转义码红色高亮显示日志
            print(f"\033[91m[Aria2DownloadManager] 已强制设定文件名: {filename}\033[0m")

            print(f"\033[91m[Aria2DownloadManager] 添加任务到 RPC: URL={url}, Options={json.dumps(aria2_options, ensure_ascii=False)}\033[0m")

            try:
                gid = self.rpc_client.add_uri([url], aria2_options)
                return {
                    'success': True,
                    'gid': gid,
                    'message': '下载任务已添加到 Aria2 RPC',
                    'filename': filename,
                    'download_dir': self.download_dir
                }
            except Exception as e:
                # RPC 失败，不再尝试本地下载
                print(f"[Aria2DownloadManager] RPC 添加失败: {e}")
                return {
                    'success': False,
                    'message': f'Aria2 RPC 连接失败，请检查服务是否启动: {str(e)}'
                }
        else:
            # RPC 不可用
            print(f"[Aria2DownloadManager] RPC 不可用")
            return {
                'success': False,
                'message': 'Aria2 RPC 服务不可用，请检查设置'
            }
    
    def get_download_status(self, gid: str) -> Dict[str, Any]:
        """获取下载任务状态（优先尝试 RPC，失败则尝试本地下载器）"""
        # 如果是本地 gid（以 local_ 开头），直接使用本地下载器
        if gid.startswith('local_'):
            return self.local_downloader.get_download_status(gid)
        
        # 尝试 RPC 方式
        if self.rpc_client:
            try:
                status = self.rpc_client.tell_status(gid, [
                    'gid', 'status', 'totalLength', 'completedLength',
                    'downloadSpeed', 'errorCode', 'errorMessage', 'files'
                ])
                
                total_length = int(status.get('totalLength', 0))
                completed_length = int(status.get('completedLength', 0))
                download_speed = int(status.get('downloadSpeed', 0))
                
                progress_percent = 0
                if total_length > 0:
                    progress_percent = int((completed_length / total_length) * 100)
                
                return {
                    'success': True,
                    'gid': gid,
                    'status': status.get('status'),
                    'total_size': total_length,
                    'downloaded_size': completed_length,
                    'progress_percent': progress_percent,
                    'speed': download_speed,
                    'error_message': status.get('errorMessage'),
                    # 改进文件名获取逻辑：确保能获取到真实文件名
                    'filename': self._extract_filename(status)
                }
            except Exception as e:
                # RPC 失败，尝试本地下载器
                return self.local_downloader.get_download_status(gid)
        else:
            # 使用本地下载器
            return self.local_downloader.get_download_status(gid)

    def _extract_filename(self, status):
        """从 Aria2 状态中提取文件名"""
        try:
            files = status.get('files', [])
            if files and len(files) > 0:
                path = files[0].get('path', '')
                if path:
                    filename = os.path.basename(path)
                    # 确保文件名有效且不是路径
                    if filename and '.' in filename and len(filename) > 4:
                        return filename
            
            # 如果无法从文件路径获取，尝试从下载链接获取
            if 'uris' in status and status['uris']:
                uri = status['uris'][0].get('uri', '')
                if uri:
                    from urllib.parse import urlparse, unquote
                    parsed = urlparse(uri)
                    filename = os.path.basename(unquote(parsed.path))
                    if filename and '.' in filename and len(filename) > 4:
                        return filename
                        
            return 'Unknown'
        except:
            return 'Unknown'
    
    def remove_download(self, gid: str) -> bool:
        """移除下载任务"""
        if self.use_rpc and self.rpc_client:
            try:
                self.rpc_client._call('aria2.remove', [gid])
                return True
            except:
                return False
        else:
            return self.local_downloader.remove_download(gid)


# 全局 Aria2 下载管理器实例
_aria2_manager = None

def get_aria2_manager(download_dir: str = None, use_rpc: bool = True,
                      host: str = 'localhost', port: int = 6800, secret: str = '') -> Aria2DownloadManager:
    """获取全局 Aria2 下载管理器"""
    global _aria2_manager
    if _aria2_manager is None:
        if download_dir is None:
            from config import Config
            download_dir = Config.DOWNLOAD_DIR
        _aria2_manager = Aria2DownloadManager(download_dir, use_rpc, host, port, secret)
    return _aria2_manager

def init_aria2_manager(download_dir: str, use_rpc: bool = True,
                       host: str = 'localhost', port: int = 6800, secret: str = ''):
    """
    初始化全局 Aria2 下载管理器
    """
    global _aria2_manager
    _aria2_manager = Aria2DownloadManager(download_dir, use_rpc, host, port, secret)
    return _aria2_manager
