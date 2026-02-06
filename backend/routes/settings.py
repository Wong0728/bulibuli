from flask import request, jsonify
from . import settings_bp
from models import Setting

@settings_bp.route('/get', methods=['GET'])
def get_settings():
    """获取所有设置"""
    try:
        # 获取所有设置（已自动合并默认值）
        all_settings = Setting.get_all_settings()
        
        return jsonify({
            'success': True,
            'settings': all_settings
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'获取设置失败: {str(e)}'})

@settings_bp.route('/save', methods=['POST'])
def save_settings():
    """保存设置"""
    data = request.get_json()
    
    try:
        # 保存所有设置
        for key, value in data.items():
            Setting.set_setting(key, value)
        
        return jsonify({'success': True, 'message': '设置已保存'})
    except Exception as e:
        return jsonify({'success': False, 'message': f'保存失败: {str(e)}'})

@settings_bp.route('/reset', methods=['POST'])
def reset_settings():
    """恢复默认设置"""
    try:
        default_settings = Setting.get_default_settings()
        
        # 保存默认设置
        for key, value in default_settings.items():
            Setting.set_setting(key, value)
        
        return jsonify({
            'success': True,
            'message': '已恢复默认设置',
            'settings': default_settings
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'重置失败: {str(e)}'})
