# -*- mode: python ; coding: utf-8 -*-

import sys
import os
from PyInstaller.utils.hooks import collect_data_files

datas = [
    ('css', 'css'),
    ('js', 'js'),
    ('index.html', '.'),
    ('bilibili.ico', '.'),
    ('resources/ffmpeg.exe', 'resources')
]


block_cipher = None

# --- PyInstaller 配置 ---
a = Analysis(['start_server.py'],
             pathex=['.', 'backend'],
             binaries=[],
             datas=datas,
             hiddenimports=['engineio.async_drivers.threading', 'flask_socketio'],
             hookspath=[],
             runtime_hooks=[],
             excludes=[],
             win_no_prefer_redirects=False,
             win_private_assemblies=False,
             cipher=block_cipher,
             noarchive=False)

pyz = PYZ(a.pure, a.zipped_data,
             cipher=block_cipher)

exe = EXE(pyz,
          a.scripts,
          a.binaries,
          a.zipfiles,
          a.datas,
          [],
          name='BilibiliUIDBuild',
          debug=False,
          bootloader_ignore_signals=False,
          strip=False,
          upx=True,
          upx_dir=os.path.abspath('.'), # 指定 upx.exe 所在目录
          runtime_tmpdir=None,
          console=True,
          icon='bilibili.ico')

coll = COLLECT(exe,
               a.binaries,
               a.zipfiles,
               a.datas,
               strip=False,
               upx=True,
               upx_dir='./',
               name='BilibiliUIDBuild')
