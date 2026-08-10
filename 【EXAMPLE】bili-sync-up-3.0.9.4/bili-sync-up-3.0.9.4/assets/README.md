# aria2 二进制文件

这个目录需要包含aria2的二进制文件，用于集成到bili-sync中。

## 如何获取aria2二进制文件

### 方法1：从官方Github下载

1. 访问 https://github.com/aria2/aria2/releases
2. 下载对应平台的二进制文件：
   - Windows: `aria2-x.x.x-win-64bit-build1.zip`
   - Linux: `aria2-x.x.x-linux-gnu-64bit-build1.tar.bz2`
   - macOS: `aria2-x.x.x-osx-darwin-build1.dmg`

### 方法2：使用包管理器安装后复制

#### Windows (使用 winget 或 chocolatey)
```powershell
# 使用 winget
winget install aria2.aria2

# 或使用 chocolatey
choco install aria2

# 然后从安装目录复制 aria2c.exe
```

#### Ubuntu/Debian
```bash
sudo apt install aria2
# 复制 /usr/bin/aria2c 到这个目录
```

#### CentOS/RHEL
```bash
sudo yum install aria2
# 或
sudo dnf install aria2
# 复制 /usr/bin/aria2c 到这个目录
```

#### macOS
```bash
brew install aria2
# 复制 /opt/homebrew/bin/aria2c (Apple Silicon) 或 /usr/local/bin/aria2c (Intel) 到这个目录
```

## 文件命名要求

请将下载的aria2二进制文件重命名为：
- Windows: `aria2c.exe`
- Linux: `aria2c`
- macOS: `aria2c`

## 验证

确保二进制文件可执行：
```bash
# Linux/macOS
chmod +x aria2c
./aria2c --version

# Windows
aria2c.exe --version
```

## 注意事项

1. 确保下载的是与您的系统架构匹配的版本（x64/x86）
2. 某些防病毒软件可能会误报aria2，请添加白名单
3. 如果您的系统中已经安装了aria2，可以直接复制已有的二进制文件

## 自动下载脚本

如果您不想手动下载，可以运行以下脚本：

### Windows PowerShell
```powershell
# 创建自动下载脚本 - 请查看 scripts/download-aria2.ps1
```

### Linux/macOS
```bash
# 创建自动下载脚本 - 请查看 scripts/download-aria2.sh
``` 