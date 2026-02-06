import re
from datetime import datetime

def format_file_size(bytes_size):
    """格式化文件大小"""
    if bytes_size == 0:
        return '0 B'
    
    units = ['B', 'KB', 'MB', 'GB', 'TB']
    k = 1024
    i = 0
    
    while bytes_size >= k and i < len(units) - 1:
        bytes_size /= k
        i += 1
    
    return f"{bytes_size:.2f} {units[i]}"

def format_speed(bytes_per_second):
    """格式化下载速度"""
    return f"{format_file_size(bytes_per_second)}/s"

def escape_html(text):
    """转义HTML特殊字符"""
    if not text:
        return ''
    return (text
            .replace('&', '&amp;')
            .replace('<', '&lt;')
            .replace('>', '&gt;')
            .replace('"', '&quot;')
            .replace("'", '&#039;'))

def sanitize_filename(filename):
    """清理文件名，移除非法字符"""
    # Windows非法字符: < > : " / \ | ? *
    illegal_chars = r'[<>:"/\\|?*]'
    filename = re.sub(illegal_chars, '_', filename)
    # 移除控制字符
    filename = re.sub(r'[\x00-\x1f\x7f-\x9f]', '', filename)
    # 限制长度
    if len(filename) > 200:
        name, ext = filename.rsplit('.', 1) if '.' in filename else (filename, '')
        filename = name[:200] + ('.' + ext if ext else '')
    return filename.strip()

def timestamp_to_datetime(timestamp):
    """时间戳转换为datetime对象"""
    if isinstance(timestamp, (int, float)):
        return datetime.fromtimestamp(timestamp)
    return None

def datetime_to_string(dt, fmt='%Y-%m-%d %H:%M:%S'):
    """datetime对象转换为字符串"""
    if dt:
        return dt.strftime(fmt)
    return None
