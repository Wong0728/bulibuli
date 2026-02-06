#!/usr/bin/env python3
"""
B站视频监控助手 - 启动脚本
"""
import sys
import os
import threading
import time
import requests
import webbrowser

# 获取程序根目录
def get_base_path():
    if hasattr(sys, '_MEIPASS'):
        return sys._MEIPASS
    return os.path.dirname(os.path.abspath(__file__))

# 添加backend目录到路径
base_path = get_base_path()
backend_dir = os.path.join(base_path, 'backend')
sys.path.insert(0, backend_dir)

# 导入并运行主程序
from app import create_app, socketio

def wait_for_server(url, timeout=30):
    """等待服务器启动并打印欢迎信息"""
    start_time = time.time()
    while time.time() - start_time < timeout:
        try:
            # 尝试访问健康检查接口
            response = requests.get(f"{url}/api/health", timeout=1)
            if response.status_code == 200:
                print("\n" + "=" * 50)
                print("B站视频监控助手已就绪！")
                print("-" * 50)
                print(f"访问地址: {url}")
                print("=" * 50)
                
                # 自动打开浏览器
                try:
                    webbrowser.open(url)
                except Exception as e:
                    print(f"[提示] 无法自动打开浏览器: {e}")
                
                return True
        except Exception:
            pass
        time.sleep(0.5)
    
    print("\n[错误] 服务器启动超时，请检查日志。")
    return False

if __name__ == '__main__':
    is_bundle = hasattr(sys, '_MEIPASS')
    
    # 禁用 Flask 的启动 banner 以保持输出清爽
    if not is_bundle:
        os.environ['WERKZEUG_RUN_MAIN'] = 'true' if os.environ.get('WERKZEUG_RUN_MAIN') == 'true' else ''
    
    app = create_app()
    
    # 获取配置的端口
    port = 5000
    url = f"http://localhost:{port}"
    
    # 只有在主进程中启动检测线程
    if is_bundle or os.environ.get('WERKZEUG_RUN_MAIN') != 'true':
        print("\n正在启动服务，请稍候...")
        print("请等待几秒，初始化完成后会自动为您打开浏览器。")
        threading.Thread(target=wait_for_server, args=(url,), daemon=True).start()
    
    # 使用 socketio.run 替代 app.run 以支持 WebSocket
    # 打包后禁用 debug 模式
    socketio.run(app, host='0.0.0.0', port=port, debug=not is_bundle, allow_unsafe_werkzeug=True, log_output=False)