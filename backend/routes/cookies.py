from flask import request, jsonify
from . import cookies_bp
from services.bili_api import get_bili_api
from models import Setting

@cookies_bp.route('/test', methods=['POST'])
def test_cookies():
    """测试Cookies是否有效"""
    data = request.get_json()
    cookies = data.get('cookies', '').strip()
    
    if not cookies:
        return jsonify({'success': False, 'message': '请提供Cookies'})
    
    result = get_bili_api().test_cookies(cookies)
    return jsonify(result)

@cookies_bp.route('/save', methods=['POST'])
def save_cookies():
    """保存Cookies"""
    data = request.get_json()
    cookies = data.get('cookies', '').strip()
    
    try:
        Setting.set_setting('cookies', cookies)
        return jsonify({'success': True, 'message': 'Cookies已保存'})
    except Exception as e:
        return jsonify({'success': False, 'message': f'保存失败: {str(e)}'})

@cookies_bp.route('/load', methods=['GET'])
def load_cookies():
    """加载已保存的Cookies"""
    try:
        cookies = Setting.get_setting('cookies', '')
        return jsonify({'success': True, 'cookies': cookies})
    except Exception as e:
        return jsonify({'success': False, 'message': f'加载失败: {str(e)}'})

@cookies_bp.route('/qrcode/generate', methods=['GET'])
def generate_qrcode():
    """获取登录二维码"""
    result = get_bili_api().get_qrcode_url()
    return jsonify(result)

@cookies_bp.route('/qrcode/poll', methods=['GET'])
def poll_qrcode():
    """轮询扫码状态"""
    qrcode_key = request.args.get('qrcode_key')
    if not qrcode_key:
        return jsonify({'success': False, 'message': '缺少qrcode_key'})
    
    result = get_bili_api().check_qrcode_status(qrcode_key)
    return jsonify(result)
