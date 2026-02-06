from flask import request, jsonify
from . import logs_bp
from models import db, Log

@logs_bp.route('/get', methods=['GET'])
def get_logs():
    """获取系统日志"""
    try:
        limit = request.args.get('limit', 100, type=int)
        
        logs = Log.query.filter_by(uid=None).order_by(
            Log.created_at.desc()
        ).limit(limit).all()
        
        # 反转顺序，最新的在最后
        logs.reverse()
        
        return jsonify({
            'success': True,
            'logs': [log.to_dict() for log in logs]
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'获取日志失败: {str(e)}'})

@logs_bp.route('/blogger', methods=['GET'])
def get_blogger_logs():
    """获取博主日志"""
    uid = request.args.get('uid', '').strip()
    limit = request.args.get('limit', 100, type=int)
    
    if not uid:
        return jsonify({'success': False, 'message': '请提供博主UID'})
    
    try:
        logs = Log.query.filter_by(uid=uid).order_by(
            Log.created_at.desc()
        ).limit(limit).all()
        
        # 反转顺序
        logs.reverse()
        
        return jsonify({
            'success': True,
            'logs': [log.to_dict() for log in logs]
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'获取日志失败: {str(e)}'})

@logs_bp.route('/clear', methods=['POST'])
def clear_logs():
    """清空日志"""
    try:
        Log.query.delete()
        db.session.commit()
        return jsonify({'success': True, 'message': '日志已清空'})
    except Exception as e:
        db.session.rollback()
        return jsonify({'success': False, 'message': f'清空失败: {str(e)}'})
