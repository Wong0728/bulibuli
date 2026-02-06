def parse_cookies(cookies_str):
    """解析cookies字符串为字典"""
    cookies = {}
    if not cookies_str:
        return cookies
    
    for item in cookies_str.split(';'):
        item = item.strip()
        if '=' in item:
            key, value = item.split('=', 1)
            cookies[key.strip()] = value.strip()
    return cookies

def cookies_to_string(cookies_dict):
    """将cookies字典转换为字符串"""
    return '; '.join([f"{k}={v}" for k, v in cookies_dict.items()])
