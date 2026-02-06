from flask import request, jsonify
from . import blogger_bp
from models import db, Blogger

@blogger_bp.route('/list', methods=['GET'])
def list_bloggers():
    """获取博主列表"""
    bloggers = Blogger.query.all()
    return jsonify({
        'success': True,
        'bloggers': [b.to_dict() for b in bloggers]
    })

@blogger_bp.route('/add', methods=['POST'])
def add_blogger():
    """添加博主"""
    data = request.get_json()
    
    uid = data.get('uid', '').strip()
    name = data.get('name', '').strip()
    min_interval = data.get('min_interval')
    max_interval = data.get('max_interval')
    
    # 验证并设置默认值
    if min_interval is None:
        min_interval = 60
    else:
        try:
            min_interval = int(min_interval)
            if min_interval < 30:
                return jsonify({'success': False, 'message': '最小间隔不能小于30秒'})
        except (ValueError, TypeError):
            min_interval = 60

    if max_interval is None:
        max_interval = 300
    else:
        try:
            max_interval = int(max_interval)
            if max_interval < 30:
                return jsonify({'success': False, 'message': '最大间隔不能小于30秒'})
        except (ValueError, TypeError):
            max_interval = 300
    if min_interval > max_interval:
        return jsonify({'success': False, 'message': '最小间隔不能大于最大间隔'})
    
    if not uid:
        return jsonify({'success': False, 'message': '请输入博主UID'})
    
    # 检查是否已存在
    existing = Blogger.query.filter_by(uid=uid).first()
    if existing:
        return jsonify({'success': False, 'message': '该博主已在监控列表中'})
    
    try:
        blogger = Blogger(
            uid=uid,
            name=name,
            min_interval=min_interval,
            max_interval=max_interval
        )
        db.session.add(blogger)
        db.session.commit()
        
        return jsonify({
            'success': True,
            'message': '博主已添加',
            'blogger_id': blogger.id
        })
    except Exception as e:
        db.session.rollback()
        print(f"[Blogger] 添加博主失败: {e}")
        return jsonify({'success': False, 'message': '添加失败，服务器内部错误'})

@blogger_bp.route('/update', methods=['POST'])
def update_blogger():
    """更新博主配置"""
    data = request.get_json()
    
    blogger_id = data.get('id')
    uid = data.get('uid', '').strip()
    name = data.get('name')
    min_interval = data.get('min_interval')
    max_interval = data.get('max_interval')
    
    if not blogger_id:
        return jsonify({'success': False, 'message': '请提供博主ID'})
    
    blogger = Blogger.query.get(blogger_id)
    if not blogger:
        return jsonify({'success': False, 'message': '未找到该博主'})
    
    try:
        if uid:
            # 检查UID是否被其他博主使用
            existing = Blogger.query.filter_by(uid=uid).first()
            if existing and existing.id != blogger_id:
                return jsonify({'success': False, 'message': '该UID已被其他博主使用'})
            blogger.uid = uid
        
        if name is not None:
            blogger.name = name.strip()
        
        if min_interval is not None:
            try:
                min_interval = int(min_interval)
                if min_interval < 30:
                    return jsonify({'success': False, 'message': '最小间隔不能小于30秒'})
            except ValueError:
                return jsonify({'success': False, 'message': '间隔必须是数字'})
            blogger.min_interval = min_interval
            
        if max_interval is not None:
            try:
                max_interval = int(max_interval)
                if max_interval < 30:
                    return jsonify({'success': False, 'message': '最大间隔不能小于30秒'})
            except ValueError:
                return jsonify({'success': False, 'message': '间隔必须是数字'})
            blogger.max_interval = max_interval
            
        if blogger.min_interval > blogger.max_interval:
            return jsonify({'success': False, 'message': '最小间隔不能大于最大间隔'})
        
        db.session.commit()
        return jsonify({'success': True, 'message': '博主配置已更新'})
    except Exception as e:
        db.session.rollback()
        print(f"[Blogger] 更新博主失败: {e}")
        return jsonify({'success': False, 'message': '更新失败，服务器内部错误'})

@blogger_bp.route('/delete', methods=['POST'])
def delete_blogger():
    """删除博主"""
    data = request.get_json()
    
    blogger_id = data.get('id')
    
    if not blogger_id:
        return jsonify({'success': False, 'message': '请提供博主ID'})
    
    blogger = Blogger.query.get(blogger_id)
    if not blogger:
        return jsonify({'success': False, 'message': '未找到该博主'})
    
    try:
        db.session.delete(blogger)
        db.session.commit()
        return jsonify({'success': True, 'message': '博主已删除'})
    except Exception as e:
        db.session.rollback()
        return jsonify({'success': False, 'message': f'删除失败: {str(e)}'})

@blogger_bp.route('/save_config', methods=['POST'])
def save_config():
    """保存博主配置（导出）"""
    try:
        bloggers = Blogger.query.all()
        configs = [b.to_dict() for b in bloggers]
        return jsonify({
            'success': True,
            'message': '配置已保存',
            'config': configs
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'保存失败: {str(e)}'})

@blogger_bp.route('/load_config', methods=['GET'])
def load_config():
    """加载博主配置（导入）"""
    # 这里只是返回当前配置，实际导入需要通过add/update接口
    try:
        bloggers = Blogger.query.all()
        return jsonify({
            'success': True,
            'bloggers': [b.to_dict() for b in bloggers]
        })
    except Exception as e:
        return jsonify({'success': False, 'message': f'加载失败: {str(e)}'})
