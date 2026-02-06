from flask import request, jsonify
from . import video_bp
from services.bili_api import get_bili_api
from models import Setting

@video_bp.route('/get_videos', methods=['POST'])
def get_videos():
    """获取UP主视频列表"""
    data = request.get_json()
    
    uid = data.get('uid', '').strip()
    cookies = data.get('cookies', '').strip()
    # 从数据库读取手动查询限制
    settings = Setting.get_all_settings()
    default_limit = settings.get('query').get('manual_query_limit')
    limit = data.get('limit', default_limit)
    
    if not uid:
        return jsonify({'success': False, 'message': '请输入用户UID'})
    
    try:
        uid_int = int(uid)
    except ValueError:
        return jsonify({'success': False, 'message': 'UID必须是数字'})
    
    # 调用B站API获取视频
    result = get_bili_api().get_user_videos(uid_int, cookies, limit=limit)
    
    return jsonify(result)

@video_bp.route('/get_video_urls', methods=['POST'])
def get_video_urls():
    """获取视频下载链接列表（支持多清晰度）"""
    data = request.get_json()

    bvid = data.get('bvid', '').strip()
    cookies = data.get('cookies', '').strip()
    # 从数据库读取视频格式设置
    settings = Setting.get_all_settings()
    default_fnval = settings.get('query').get('video_format')
    fnval = data.get('fnval', default_fnval)
    
    if not bvid:
        return jsonify({'success': False, 'message': '请提供视频BV号'})
    
    # 调用获取视频链接的函数
    result = get_bili_api().get_video_urls(bvid, cookies, fnval=fnval)
    
    return jsonify(result)

@video_bp.route('/get_audio_url', methods=['POST'])
def get_audio_url():
    """获取音频下载链接"""
    data = request.get_json()
    
    bvid = data.get('bvid', '').strip()
    cookies = data.get('cookies', '').strip()
    
    if not bvid:
        return jsonify({'success': False, 'message': '请提供视频BV号'})
    
    # 调用获取音频链接的函数
    result = get_bili_api().get_audio_url(bvid, cookies)
    
    return jsonify(result)
