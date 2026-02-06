import requests
import hashlib
import urllib.parse
import time
from functools import reduce

# WBI签名相关
mixinKeyEncTab = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49,
    33, 9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40,
    61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25, 54, 21, 56, 59, 6, 63, 57, 62, 11,
    36, 20, 34, 44, 52
]

# 质量名称映射
QUALITY_NAMES = {
    127: '8K 超高清',
    126: '杜比视界',
    125: 'HDR 真彩',
    120: '4K 超清',
    116: '1080P60 高帧率',
    112: '1080P+ 高码率',
    80: '1080P 高清',
    74: '720P60 高帧率',
    64: '720P 高清',
    32: '480P 清晰',
    16: '360P 流畅'
}

class BiliAPI:
    def __init__(self):
        self.session = requests.Session()
        self.session.headers.update({
            'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
            'Referer': 'https://www.bilibili.com/'
        })
    
    def _get_mixin_key(self, orig):
        """对 imgKey 和 subKey 进行字符顺序打乱编码"""
        return reduce(lambda s, i: s + orig[i], mixinKeyEncTab, '')[:32]
    
    def _get_wbi_keys(self):
        """获取最新的 img_key 和 sub_key"""
        try:
            resp = self.session.get('https://api.bilibili.com/x/web-interface/nav', timeout=10, verify=False)
            resp.raise_for_status()
            json_content = resp.json()

            # 即使code不为0(如-101账号未登录)，也可能返回wbi_img
            data = json_content.get('data', {})
            wbi_img = data.get('wbi_img', {})

            if not wbi_img:
                print(f"获取WBI密钥失败: 响应中无wbi_img数据, code={json_content.get('code')}")
                return None, None

            img_url = wbi_img.get('img_url', '')
            sub_url = wbi_img.get('sub_url', '')

            if not img_url or not sub_url:
                print(f"获取WBI密钥失败: img_url或sub_url为空")
                return None, None

            img_key = img_url.rsplit('/', 1)[1].split('.')[0]
            sub_key = sub_url.rsplit('/', 1)[1].split('.')[0]
            return img_key, sub_key
        except Exception as e:
            print(f"获取WBI密钥失败: {e}")
            return None, None
    
    def _enc_wbi(self, params, img_key, sub_key):
        """为请求参数进行 wbi 签名"""
        mixin_key = self._get_mixin_key(img_key + sub_key)
        curr_time = round(time.time())
        params['wts'] = curr_time
        params = dict(sorted(params.items()))
        params = {
            k: ''.join(filter(lambda chr: chr not in "!'()*", str(v)))
            for k, v in params.items()
        }
        query = urllib.parse.urlencode(params)
        wbi_sign = hashlib.md5((query + mixin_key).encode()).hexdigest()
        params['w_rid'] = wbi_sign
        return params
    
    def _make_request(self, url, params=None, cookies=None, headers=None):
        """发送HTTP请求"""
        try:
            req_headers = dict(self.session.headers)
            if headers:
                req_headers.update(headers)
            
            req_cookies = {}
            if cookies:
                if isinstance(cookies, str):
                    for item in cookies.split(';'):
                        item = item.strip()
                        if '=' in item:
                            k, v = item.split('=', 1)
                            req_cookies[k.strip()] = v.strip()
                else:
                    req_cookies = cookies
            
            response = self.session.get(url, params=params, headers=req_headers, cookies=req_cookies, timeout=10)
            response.raise_for_status()
            return response.json()
        except requests.exceptions.RequestException as e:
            print(f"[BiliAPI] 网络请求错误: {e}")
            return {'success': False, 'message': '网络请求失败，请检查网络连接'}
        except Exception as e:
            print(f"[BiliAPI] 内部错误: {e}")
            return {'success': False, 'message': '服务器内部错误'}
    
    def get_user_videos(self, uid, cookies=None, limit=10):
        """获取用户视频列表"""
        try:
            # 获取WBI密钥
            img_key, sub_key = self._get_wbi_keys()
            if not img_key or not sub_key:
                return {'success': False, 'message': '获取WBI密钥失败'}
            
            # 构建请求参数
            params = {
                'mid': uid,
                'ps': limit,
                'order': 'pubdate',
                'platform': 'web',
                'web_location': '1550101'
            }
            
            # 添加WBI签名
            signed_params = self._enc_wbi(params, img_key, sub_key)
            
            # 构建完整URL
            base_url = "https://api.bilibili.com/x/space/wbi/arc/search"
            
            # 设置请求头
            headers = {
                'Referer': f'https://space.bilibili.com/{uid}',
                'Accept': 'application/json, text/plain, */*',
            }
            
            # 发送请求
            data = self._make_request(base_url, signed_params, cookies, headers)
            
            # 检查是否是错误响应（由_make_request返回）
            if data.get('success') is False and 'message' in data:
                return data  # 直接返回错误信息
            
            # 检查B站API返回状态 (code为0表示成功)
            code = data.get('code')
            if code is None:
                return {'success': False, 'message': 'B站API返回格式异常: 缺少code字段', 'debug': data}
            if int(code) != 0:
                return {'success': False, 'message': f"B站API错误: {data.get('message', '未知错误')}"}
            
            # 提取视频信息
            videos = []
            vlist = data.get('data', {}).get('list', {}).get('vlist', [])
            
            for video in vlist:
                videos.append({
                    'title': video.get('title', ''),
                    'bvid': video.get('bvid', ''),
                    'aid': video.get('aid', ''),
                    'url': f"https://www.bilibili.com/video/{video.get('bvid', '')}",
                    'pic': video.get('pic', ''),
                    'play': video.get('play', 0),
                    'comment': video.get('comment', 0),
                    'created': video.get('created', 0),
                    'length': video.get('length', ''),
                    'description': video.get('description', '')
                })
            
            return {
                'success': True,
                'videos': videos,
                'total': data.get('data', {}).get('page', {}).get('count', 0)
            }
            
        except Exception as e:
            return {'success': False, 'message': f'处理错误: {str(e)}'}
    
    def get_video_info(self, bvid, cookies=None):
        """获取视频基本信息，包括cid"""
        try:
            headers = {
                'Referer': f'https://www.bilibili.com/video/{bvid}',
                'Accept': 'application/json, text/plain, */*',
            }
            
            url = f"https://api.bilibili.com/x/web-interface/view?bvid={bvid}"
            data = self._make_request(url, cookies=cookies, headers=headers)
            
            if data.get('code') != 0:
                return {'success': False, 'message': f"获取视频信息失败: {data.get('message', '未知错误')}"}
            
            video_data = data.get('data', {})
            return {
                'success': True,
                'cid': video_data.get('cid'),
                'title': video_data.get('title'),
                'duration': video_data.get('duration'),
                'owner': video_data.get('owner', {}),
                'stat': video_data.get('stat', {})
            }
            
        except Exception as e:
            return {'success': False, 'message': f'处理错误: {str(e)}'}
    
    def get_video_urls(self, bvid, cookies=None, fnval=4048, preferred_quality=None):
        """获取视频下载链接列表（支持多清晰度）
        
        Args:
            bvid: 视频BV号
            cookies: Cookies字符串
            fnval: 视频流格式标识，默认4048获取所有DASH格式
            preferred_quality: 首选画质，如果视频支持则选择最接近但不超过的，否则选最高可用
        """
        try:
            # 首先获取视频的cid
            info_result = self.get_video_info(bvid, cookies)
            if not info_result.get('success'):
                return info_result
            
            cid = info_result.get('cid')
            if not cid:
                return {'success': False, 'message': '未找到视频cid'}
            
            # 构建请求参数 - 使用fnval=4048获取所有可用画质
            params = {
                'bvid': bvid,
                'cid': cid,
                'qn': 127,  # 请求最高画质8K，让API返回所有可用画质
                'fnval': fnval,  # 4048获取所有DASH格式
                'fnver': 0,
                'fourk': 1,
                'platform': 'web',
                'web_location': '1550101'
            }
            
            # 使用旧端点（与demo.py一致，更稳定）
            base_url = "https://api.bilibili.com/x/player/playurl"
            
            headers = {
                'Referer': f'https://www.bilibili.com/video/{bvid}',
                'Accept': 'application/json, text/plain, */*',
            }
            
            data = self._make_request(base_url, params, cookies, headers)
            
            if data.get('code') != 0:
                return {'success': False, 'message': f"B站API错误: {data.get('message', '未知错误')}"}
            
            # 提取视频流信息
            dash = data.get('data', {}).get('dash', {})
            video_streams = dash.get('video', [])
            
            # 获取API返回的支持画质列表
            accept_quality = data.get('data', {}).get('accept_quality', [])
            
            if not video_streams:
                # 如果没有dash格式，尝试获取flv格式
                durl = data.get('data', {}).get('durl', [])
                if durl:
                    video_qualities = []
                    for stream in durl:
                        video_qualities.append({
                            'quality': 80,
                            'quality_name': '流畅',
                            'width': 1280,
                            'height': 720,
                            'url': stream.get('url'),
                            'size': stream.get('size', 0),
                            'format': 'flv'
                        })
                    return {
                        'success': True,
                        'qualities': video_qualities
                    }
                return {'success': False, 'message': '未找到视频流'}
            
            # 处理视频流
            video_qualities = []
            available_qualities = set()  # 记录可用的画质代码
            
            for stream in video_streams:
                quality = stream.get('id', 80)
                width = stream.get('width', 1280)
                height = stream.get('height', 720)
                size = stream.get('size', 0)
                base_url_stream = stream.get('baseUrl')
                
                if not base_url_stream:
                    continue
                
                available_qualities.add(quality)
                
                # 获取视频格式
                mime_type = stream.get('mimeType', '')
                codec = stream.get('codecs', '')
                
                if 'video/mp4' in mime_type or 'avc' in codec:
                    fmt = 'm4s'
                elif 'video/webm' in mime_type or 'hevc' in codec:
                    fmt = 'm4s'
                else:
                    fmt = 'm4s'
                
                video_qualities.append({
                    'quality': quality,
                    'quality_name': QUALITY_NAMES.get(quality, f'{width}x{height}'),
                    'width': width,
                    'height': height,
                    'url': base_url_stream,
                    'size': size,
                    'format': fmt
                })
            
            # 按质量从高到低排序
            video_qualities.sort(key=lambda x: x['quality'], reverse=True)
            
            # 如果有首选画质，选择最合适的
            selected_quality = None
            if preferred_quality and video_qualities:
                # 首选画质存在，找最接近但不超过首选画质的
                for q in video_qualities:
                    if q['quality'] <= preferred_quality:
                        selected_quality = q
                        break
                # 如果没有找到（所有画质都高于首选），选择最高画质
                if not selected_quality:
                    selected_quality = video_qualities[0]
            
            return {
                'success': True,
                'qualities': video_qualities,
                'selected_quality': selected_quality,
                'available_qualities': list(available_qualities),
                'accept_quality': accept_quality
            }
            
        except Exception as e:
            return {'success': False, 'message': f'处理错误: {str(e)}'}
    
    def get_audio_url(self, bvid, cookies=None):
        """获取音频下载链接"""
        try:
            # 首先获取视频的cid
            info_result = self.get_video_info(bvid, cookies)
            if not info_result.get('success'):
                return info_result

            cid = info_result.get('cid')
            if not cid:
                return {'success': False, 'message': '未找到视频cid'}

            # 构建请求参数 - 使用 DASH 格式获取音频 (fnval=16)
            params = {
                'bvid': bvid,
                'cid': cid,
                'qn': 80,
                'fnval': 16,  # DASH格式
                'fnver': 0,
                'fourk': 1,
                'platform': 'web',
                'web_location': '1550101'
            }
            
            # 使用旧端点（与demo.py一致，更稳定）
            base_url = "https://api.bilibili.com/x/player/playurl"
            
            headers = {
                'Referer': f'https://www.bilibili.com/video/{bvid}',
                'Accept': 'application/json, text/plain, */*',
            }
            
            data = self._make_request(base_url, params, cookies, headers)
            
            if data.get('code') != 0:
                return {'success': False, 'message': f"B站API错误: {data.get('message', '未知错误')}"}
            
            # 提取音频信息
            dash = data.get('data', {}).get('dash', {})
            audio_streams = dash.get('audio', [])
            
            if not audio_streams:
                return {'success': False, 'message': '未找到音频流'}
            
            # 选择第一个音频流（通常是最高质量）
            audio_stream = audio_streams[0]
            audio_url = audio_stream.get('baseUrl')
            
            if not audio_url:
                return {'success': False, 'message': '未找到音频下载链接'}
            
            # 获取音频格式
            mime_type = audio_stream.get('mimeType', '')
            codec = audio_stream.get('codecs', '')
            
            # 确定文件扩展名
            if 'audio/mp4' in mime_type or 'mp4a' in codec:
                ext = 'm4s'
            elif 'audio/webm' in mime_type or 'opus' in codec:
                ext = 'webm'
            else:
                ext = 'm4s'
            
            return {
                'success': True,
                'audio_url': audio_url,
                'ext': ext
            }
            
        except Exception as e:
            return {'success': False, 'message': f'处理错误: {str(e)}'}
    
    def test_cookies(self, cookies):
        """测试cookies是否有效"""
        try:
            url = 'https://api.bilibili.com/x/web-interface/nav'
            data = self._make_request(url, cookies=cookies)
            
            if data.get('code') == 0 and data.get('data', {}).get('isLogin'):
                return {
                    'success': True,
                    'message': 'Cookies有效',
                    'username': data.get('data', {}).get('uname', '未知用户')
                }
            else:
                return {'success': False, 'message': 'Cookies无效或已过期'}
                
        except Exception as e:
            return {'success': False, 'message': f'测试失败: {str(e)}'}

    def get_qrcode_url(self):
        """申请扫码登录二维码"""
        try:
            url = 'https://passport.bilibili.com/x/passport-login/web/qrcode/generate'
            headers = {
                'Referer': 'https://passport.bilibili.com/login',
                'Origin': 'https://passport.bilibili.com'
            }
            # 使用 self.session 以保持会话，并添加 verify=False 防止 SSL 问题
            response = self.session.get(url, headers=headers, timeout=10, verify=False)
            response.raise_for_status()
            data = response.json()
            
            if data.get('code') == 0:
                return {
                    'success': True,
                    'data': data.get('data')
                }
            else:
                return {'success': False, 'message': f"获取二维码失败: {data.get('message', '未知错误')}"}
        except Exception as e:
            print(f"[BiliAPI] 获取二维码异常: {e}")
            return {'success': False, 'message': f'获取二维码异常: {str(e)}'}

    def check_qrcode_status(self, qrcode_key):
        """检查扫码登录状态"""
        try:
            url = 'https://passport.bilibili.com/x/passport-login/web/qrcode/poll'
            params = {'qrcode_key': qrcode_key}
            headers = {
                'Referer': 'https://passport.bilibili.com/login',
                'Origin': 'https://passport.bilibili.com'
            }
            # 使用 self.session 发送请求
            response = self.session.get(url, params=params, headers=headers, timeout=10, verify=False)
            response.raise_for_status()
            data = response.json()
            
            if data.get('code') == 0:
                poll_data = data.get('data', {})
                code = poll_data.get('code')
                
                result = {
                    'success': True,
                    'code': code,
                    'message': poll_data.get('message'),
                }
                
                if code == 0:
                    # 登录成功，从会话中获取所有 B 站相关的 Cookies
                    # B 站的 Cookie 可能分布在 bilibili.com 和 passport.bilibili.com
                    cookies_dict = self.session.cookies.get_dict(domain=".bilibili.com")
                    # 如果 domain 为空，尝试获取全部
                    if not cookies_dict:
                        cookies_dict = self.session.cookies.get_dict()
                    
                    # 确保关键 Cookie 存在
                    if 'SESSDATA' in cookies_dict:
                        cookies_str = '; '.join([f"{k}={v}" for k, v in cookies_dict.items()])
                        result['cookies'] = cookies_str
                    else:
                        # 如果没有获取到，尝试直接从响应头解析（备用方案）
                        resp_cookies = response.cookies.get_dict()
                        if 'SESSDATA' in resp_cookies:
                            cookies_str = '; '.join([f"{k}={v}" for k, v in resp_cookies.items()])
                            result['cookies'] = cookies_str
                        else:
                            result['success'] = False
                            result['message'] = '登录成功但未获取到有效 Cookies'
                
                return result
            else:
                return {'success': False, 'message': f"检查状态失败: {data.get('message', '未知错误')}"}
        except Exception as e:
            print(f"[BiliAPI] 检查状态异常: {e}")
            return {'success': False, 'message': f'检查状态异常: {str(e)}'}

# 全局API实例
_bili_api = None

def get_bili_api() -> BiliAPI:
    """获取全局 BiliAPI 实例"""
    global _bili_api
    if _bili_api is None:
        _bili_api = BiliAPI()
    return _bili_api
