from flask import request, jsonify
from . import history_bp
from models import db, History

@history_bp.route('/list', methods=['GET'])
def list_history():
    """获取所有下载历史"""
    try:
        history = History.query.order_by(History.download_time.desc()).all()
        return jsonify({
            'success': True,
            'history': [h.to_dict() for h in history]
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'获取历史记录失败: {str(e)}'})

@history_bp.route('/by_uid', methods=['GET'])
def get_history_by_uid():
    """按博主获取历史"""
    uid = request.args.get('uid', '').strip()
    
    if not uid:
        return jsonify({'success': False, 'message': '请提供博主UID'})
    
    try:
        history = History.query.filter_by(uid=uid).order_by(History.download_time.desc()).all()
        return jsonify({
            'success': True,
            'history': [h.to_dict() for h in history]
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'获取历史记录失败: {str(e)}'})

@history_bp.route('/clear', methods=['POST'])
def clear_history():
    """清空历史记录"""
    try:
        History.query.delete()
        db.session.commit()
        return jsonify({'success': True, 'message': '历史记录已清空'})
    except Exception as e:
        db.session.rollback()
        return jsonify({'success': False, 'message': f'清空失败: {str(e)}'})

@history_bp.route('/search', methods=['GET'])
def search_history():
    """搜索历史记录"""
    keyword = request.args.get('keyword', '').strip().lower()
    
    if not keyword:
        return list_history()
    
    try:
        # 搜索标题、BVID、UID
        history = History.query.filter(
            db.or_(
                History.title.ilike(f'%{keyword}%'),
                History.bvid.ilike(f'%{keyword}%'),
                History.uid.ilike(f'%{keyword}%')
            )
        ).order_by(History.download_time.desc()).all()
        
        return jsonify({
            'success': True,
            'history': [h.to_dict() for h in history]
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'搜索失败: {str(e)}'})
