#!/usr/bin/env python3
"""
补哩补哩 bulibuli 构建脚本

用法:
    python build.py              # 编译并直接启动程序（测试模式）
    python build.py --portable   # 构建当前平台完整便携版
    python build.py --core       # 构建不含媒体运行时的轻量命令包
    python build.py --portable --platform linux --target x86_64-unknown-linux-gnu

产物:
    dist/bulibuli-<platform>-<arch>-portable-v<version>/   便携版目录
"""

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import time
import tomllib
import platform as host_platform
from pathlib import Path

ROOT = Path(__file__).resolve().parent

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
if hasattr(sys.stderr, "reconfigure"):
    sys.stderr.reconfigure(encoding="utf-8")

APP_SLUG = "bulibuli"
APP_DISPLAY_NAME = "补哩补哩"
APP_SLOGAN = "下架之前，先下为敬。"
with (ROOT / "Cargo.toml").open("rb") as _cargo_file:
    APP_VERSION = tomllib.load(_cargo_file)["package"]["version"]
PORTABLE_RESOURCE_DIRS = ("geo",)
PLATFORM_NAMES = {"windows", "linux", "macos", "termux"}
PACKAGE_MANIFEST_NAME = "bulibuli.package.json"


def normalize_platform(value):
    if value != "auto":
        if value not in PLATFORM_NAMES:
            raise ValueError(f"unsupported platform: {value}")
        return value
    if sys.platform == "win32":
        return "windows"
    if sys.platform == "darwin":
        return "macos"
    return "linux"


def executable_name(platform_name):
    return f"{APP_SLUG}.exe" if platform_name == "windows" else APP_SLUG


def architecture_name(target=None):
    source = (target or host_platform.machine()).lower()
    if "aarch64" in source or "arm64" in source:
        return "arm64"
    if "x86_64" in source or "amd64" in source:
        return "x86_64"
    if "i686" in source or source in {"x86", "i386"}:
        return "x86"
    return re.sub(r"[^a-z0-9]+", "-", source).strip("-") or "unknown"


def release_binary_path(platform_name, target=None):
    release_dir = ROOT / "target" / (target if target else "") / "release"
    return release_dir / executable_name(platform_name)


def package_stem(platform_name, target=None, variant="portable"):
    return f"{APP_SLUG}-{platform_name}-{architecture_name(target)}-{variant}-v{APP_VERSION}"


def run(cmd, cwd=None, check=True):
    """执行命令并实时输出"""
    print(f"  > {' '.join(cmd) if isinstance(cmd, list) else cmd}")
    result = subprocess.run(cmd, cwd=cwd, shell=isinstance(cmd, str))
    if check and result.returncode != 0:
        print(f"  [错误] 命令执行失败 (返回码 {result.returncode})")
        sys.exit(1)
    return result.returncode


def _process_executable_path(pid):
    """读取指定 PID 的实际可执行文件路径；读取失败时返回 None。"""
    if sys.platform != "win32":
        return None
    try:
        result = subprocess.run(
            [
                "powershell",
                "-NoProfile",
                "-Command",
                f"(Get-CimInstance Win32_Process -Filter 'ProcessId={int(pid)}').ExecutablePath",
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
        )
    except (FileNotFoundError, ValueError):
        return None
    path = result.stdout.strip()
    return Path(path).resolve() if path else None


def _kill_processes_by_name(exe_name, expected_path=None):
    """只终止指定可执行文件的进程，返回终止数量。

    使用 /FO CSV 输出：默认表格格式会将镜像名截断到 25 字符，
    导致长进程名匹配失败。
    """
    try:
        out = subprocess.run(
            ["tasklist", "/FI", f"IMAGENAME eq {exe_name}", "/FO", "CSV", "/NH"],
            capture_output=True,
            check=False,
        )
        # tasklist 输出使用控制台 OEM 代码页：非中文 Windows 上按 GBK 解码会乱码。
        try:
            text = out.stdout.decode("oem", errors="replace")
        except LookupError:  # 非 Windows 环境无 "oem" 编码
            text = out.stdout.decode("gbk", errors="ignore")
    except FileNotFoundError:
        return 0

    killed = 0
    for line in text.splitlines():
        fields = [f.strip('"') for f in line.strip().split('","')]
        if len(fields) < 2 or fields[0].lower() != exe_name.lower():
            continue
        pid = fields[1]
        if not pid.isdigit():
            continue
        if expected_path is not None:
            actual_path = _process_executable_path(pid)
            if actual_path is None or actual_path != expected_path:
                continue
        print(f"  [清理] 终止旧实例 PID={pid} ({exe_name})")
        subprocess.run(["taskkill", "/F", "/PID", pid], capture_output=True)
        killed += 1
    return killed


def stop_existing_instances(platform_name, target=None):
    """关闭本程序之前残留的、且路径完全匹配的运行实例。"""
    if sys.platform != "win32":
        return

    if platform_name != "windows" or target:
        return
    exe_name = executable_name(platform_name)
    expected_path = release_binary_path(platform_name, target).resolve()
    killed = _kill_processes_by_name(exe_name, expected_path)
    if killed:
        time.sleep(0.8)


def check_cargo():
    """检查 cargo 是否可用"""
    print("[1/5] 检查 cargo...")
    if shutil.which("cargo") is None:
        print("  [错误] 未找到 cargo，请先安装 Rust 工具链")
        sys.exit(1)
    print("  cargo 可用")


def build_frontend_bundle():
    """Build the optional portable bundle; development still serves raw modules."""
    frontend_dir = ROOT / "static" / "js"
    npm = "npm.cmd" if sys.platform == "win32" else "npm"
    if shutil.which(npm) is None:
        print("  [FAIL] npm is required for the frontend bundle")
        raise SystemExit(1)
    run([npm, "ci", "--ignore-scripts"], cwd=str(frontend_dir))
    run([npm, "run", "build"], cwd=str(frontend_dir))
    bundle = ROOT / "static" / "dist" / "app.bundle.js"
    if not bundle.is_file():
        print(f"  [FAIL] frontend bundle was not created: {bundle}")
        raise SystemExit(1)
    return bundle


def build_release(platform_name, target=None):
    """cargo build --release for the requested platform/target."""
    print(f"[2/5] 编译 Rust 项目 (release, {platform_name})...")
    command = ["cargo", "build", "--locked", "--release"]
    if target:
        command.extend(["--target", target])
    run(command, cwd=str(ROOT))
    exe_path = release_binary_path(platform_name, target)
    if not exe_path.exists():
        print(f"  [错误] 编译产物不存在: {exe_path}")
        sys.exit(1)
    print(f"  编译完成: {exe_path}")
    return exe_path


def write_checksum(archive_path):
    digest = hashlib.sha256()
    with archive_path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    checksum_path = archive_path.parent / f"{archive_path.name}.sha256"
    checksum_path.write_text(f"{digest.hexdigest()}  {archive_path.name}\n", encoding="utf-8")
    return checksum_path


def write_file_checksum(file_path):
    """Write a sha256 manifest next to a bundled runtime binary."""
    digest = hashlib.sha256()
    with file_path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    manifest = file_path.with_name(f"{file_path.name}.sha256")
    manifest.write_text(f"{digest.hexdigest()}  {file_path.name}\n", encoding="utf-8")
    return manifest


def sha256_file(file_path):
    digest = hashlib.sha256()
    with file_path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_package_manifest(package_dir, platform_name, target=None, variant="portable"):
    """Write package identity and per-file hashes used by installers."""
    runtime_names = (
        ("aria2c.exe", "ffmpeg.exe")
        if platform_name == "windows"
        else ("aria2c", "ffmpeg")
    )
    runtime_dir = package_dir / "resources"
    files = []
    for path in sorted(package_dir.rglob("*")):
        if not path.is_file() or path.name == PACKAGE_MANIFEST_NAME:
            continue
        files.append(
            {
                "path": path.relative_to(package_dir).as_posix(),
                "size": path.stat().st_size,
                "sha256": sha256_file(path),
            }
        )
    manifest = {
        "schema_version": 1,
        "app_version": APP_VERSION,
        "platform": platform_name,
        "architecture": architecture_name(target),
        "variant": variant,
        "files": files,
        "runtime": {
            "aria2c": (runtime_dir / runtime_names[0]).is_file(),
            "ffmpeg": (runtime_dir / runtime_names[1]).is_file(),
            "ffprobe": (runtime_dir / ("ffprobe.exe" if platform_name == "windows" else "ffprobe")).is_file(),
        },
    }
    manifest_path = package_dir / PACKAGE_MANIFEST_NAME
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return manifest_path


def _unix_runtime_dependencies(binary, platform_name):
    """Return non-system shared libraries needed by a Unix runtime binary."""
    if platform_name == "linux":
        tool = shutil.which("ldd")
        if tool is None:
            raise RuntimeError("Linux Release 组装需要 ldd 来收集 aria2c/FFmpeg 运行库")
        output = subprocess.run(
            [tool, str(binary)], capture_output=True, text=True, encoding="utf-8", check=False
        )
        if output.returncode != 0:
            raise RuntimeError(f"无法分析 Unix 运行时依赖：{output.stderr.strip()}")
        if "not found" in output.stdout:
            raise RuntimeError(f"Unix 运行时缺少动态库：{binary}")
        dependencies = []
        for line in output.stdout.splitlines():
            match = re.search(r"=>\s+(/\S+)|^\s+(/\S+)\s+\(", line)
            candidate = next((value for value in match.groups() if value), None) if match else None
            if candidate:
                dependencies.append(Path(candidate))
        # glibc 与动态加载器由用户系统提供；其它发行版库随 Release 携带。
        system_names = (
            "ld-linux",
            "libc.so",
            "libm.so",
            "libpthread.so",
            "libdl.so",
            "librt.so",
            "libresolv.so",
        )
        return [
            path
            for path in dependencies
            if path.is_file() and not path.name.startswith(system_names)
        ]

    tool = shutil.which("otool")
    if tool is None:
        raise RuntimeError("macOS Release 组装需要 otool 来收集 aria2c/FFmpeg 运行库")
    output = subprocess.run(
        [tool, "-L", str(binary)], capture_output=True, text=True, encoding="utf-8", check=False
    )
    if output.returncode != 0:
        raise RuntimeError(f"无法分析 macOS 运行时依赖：{output.stderr.strip()}")
    dependencies = []
    for line in output.stdout.splitlines()[1:]:
        candidate = line.strip().split(" ", 1)[0]
        if candidate.startswith("/") and not candidate.startswith(("/usr/lib/", "/System/")):
            path = Path(candidate)
            if path.is_file():
                dependencies.append(path)
    return dependencies


def bundle_unix_runtime(source, name, resources_dst, platform_name):
    """Bundle a Unix runtime plus loadable non-system libraries behind a stable wrapper."""
    actual = resources_dst / f"{name}.bin"
    shutil.copy2(source, actual)
    actual.chmod(actual.stat().st_mode | 0o111)
    library_dir = resources_dst / "lib"
    library_dir.mkdir(exist_ok=True)

    pending = [actual]
    copied = set()
    while pending:
        current = pending.pop()
        for dependency in _unix_runtime_dependencies(current, platform_name):
            destination = library_dir / dependency.name
            if dependency in copied:
                continue
            if destination.exists():
                # macOS install_name_tool may leave a previously bundled dylib
                # read-only; make it replaceable when aria2 and FFmpeg share it.
                destination.chmod(destination.stat().st_mode | 0o200)
            shutil.copy2(dependency, destination)
            copied.add(dependency)
            destination.chmod(destination.stat().st_mode | 0o111)
            pending.append(destination)

            if platform_name == "macos":
                install_name_tool = shutil.which("install_name_tool")
                if install_name_tool is None:
                    raise RuntimeError("macOS Release 组装需要 install_name_tool")
                subprocess.run(
                    [install_name_tool, "-id", f"@loader_path/{destination.name}", str(destination)],
                    check=True,
                )
                replacement = (
                    f"@loader_path/lib/{destination.name}"
                    if current == actual
                    else f"@loader_path/{destination.name}"
                )
                subprocess.run(
                    [install_name_tool, "-change", str(dependency), replacement, str(current)],
                    check=True,
                )

    wrapper = resources_dst / name
    if platform_name == "linux":
        wrapper_text = (
            "#!/bin/sh\n"
            'HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"\n'
            'exec env LD_LIBRARY_PATH="$HERE/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" '
            '"$HERE/' + actual.name + '" "$@"\n'
        )
    else:
        wrapper_text = (
            "#!/bin/sh\n"
            'HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"\n'
            'exec env DYLD_LIBRARY_PATH="$HERE/lib${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" '
            '"$HERE/' + actual.name + '" "$@"\n'
        )
    wrapper.write_text(wrapper_text, encoding="utf-8", newline="\n")
    wrapper.chmod(wrapper.stat().st_mode | 0o111)
    for path in [wrapper, actual, *library_dir.iterdir()]:
        write_file_checksum(path)
    return wrapper


def validate_package_tree(package_dir, platform_name, variant):
    """Fail packaging if the archive would miss a direct-run contract file."""
    binary = package_dir / executable_name(platform_name)
    required = [
        binary,
        package_dir / "README.md",
        package_dir / "static" / "index.html",
        package_dir / PACKAGE_MANIFEST_NAME,
    ]
    if platform_name in {"linux", "termux"}:
        required.append(package_dir / "install.sh")
    elif platform_name == "windows" and (package_dir / "install.ps1").exists():
        raise RuntimeError("Windows Release 不应嵌入 install.ps1；安装器由仓库单独提供")
    runtime_names = ("aria2c.exe", "ffmpeg.exe") if platform_name == "windows" else ("aria2c", "ffmpeg")
    if variant == "portable" and platform_name != "termux":
        for name in runtime_names:
            runtime = package_dir / "resources" / name
            required.extend((runtime, runtime.with_name(f"{runtime.name}.sha256")))
    else:
        bundled = [package_dir / "resources" / name for name in runtime_names]
        bundled.append(package_dir / "resources" / "lib")
        unexpected = [str(path.relative_to(package_dir)) for path in bundled if path.exists()]
        if unexpected:
            raise RuntimeError(f"轻量包不应包含媒体运行时：{', '.join(unexpected)}")
    missing = [str(path.relative_to(package_dir)) for path in required if not path.is_file()]
    if missing:
        raise RuntimeError(f"包契约不完整（{platform_name}）：{', '.join(missing)}")


def assemble_package(exe_path, platform_name, target=None, variant="portable"):
    """组装完整 portable 或不含媒体运行时的 core 归档。"""
    print(f"[3/5] 组装 {variant} 包目录 ({platform_name})...")
    dist_dir = ROOT / "dist"
    stem = package_stem(platform_name, target, variant)
    package_dir = dist_dir / stem

    if package_dir.exists():
        shutil.rmtree(package_dir)
    package_dir.mkdir(parents=True)

    binary_name = executable_name(platform_name)
    shutil.copy2(exe_path, package_dir / binary_name)
    print(f"  已复制: {binary_name}")

    for document_name in (
        "README.md",
        "LICENSE",
        "NOTICE.md",
        "CHANGELOG.md",
        "CODE_OF_CONDUCT.md",
        "SECURITY.md",
    ):
        document = ROOT / document_name
        if document.is_file():
            shutil.copy2(document, package_dir / document_name)

    resources_src = ROOT / "resources"
    if resources_src.exists():
        resources_dst = package_dir / "resources"
        resources_dst.mkdir()
        copied_resources = []
        for name in ("README.md",):
            source = resources_src / name
            if source.is_file():
                destination = resources_dst / name
                shutil.copy2(source, destination)
                copied_resources.append(name)

        if variant == "portable" and platform_name != "termux":
            runtime_names = (
                ("aria2c.exe", "ffmpeg.exe")
                if platform_name == "windows"
                else ("aria2c", "ffmpeg", "ffprobe")
            )
            if platform_name == "windows":
                for name in runtime_names:
                    source = resources_src / name
                    if not source.is_file():
                        source = Path(shutil.which(name) or "")
                    if not source.is_file():
                        raise RuntimeError(f"Windows Release 缺少可运行时：{name}")
                    destination = resources_dst / name
                    shutil.copy2(source, destination)
                    write_file_checksum(destination)
                    copied_resources.append(f"{name} (+ sha256)")
            else:
                for name in runtime_names:
                    source = Path(shutil.which(name) or "")
                    if not source.is_file():
                        if name == "ffprobe":
                            continue
                        raise RuntimeError(
                            f"无法组装可运行的 {platform_name} Release：找不到 {name}。"
                            "请先安装运行时工具后重试。"
                        )
                    bundle_unix_runtime(source, name, resources_dst, platform_name)
                    copied_resources.append(f"{name} (+ bundled libraries and sha256)")
        for name in PORTABLE_RESOURCE_DIRS:
            source = resources_src / name
            if source.is_dir():
                shutil.copytree(source, resources_dst / name)
                copied_resources.append(f"{name}/")
        print(f"  已复制: resources/ ({', '.join(copied_resources) or '空'})")
    else:
        raise RuntimeError("resources/ 目录不存在，无法组装包含 aria2c/FFmpeg 的可运行 Release")

    static_src = ROOT / "static"
    if static_src.exists():
        shutil.copytree(
            static_src,
            package_dir / "static",
            ignore=shutil.ignore_patterns(
                "node_modules",
                "test-results",
                "playwright-report",
                "*.trace.zip",
            ),
        )
        portable_index = package_dir / "static" / "index.html"
        if (package_dir / "static" / "dist" / "app.bundle.js").is_file() and portable_index.is_file():
            index_text = portable_index.read_text(encoding="utf-8")
            portable_index.write_text(
                index_text.replace("js/app.js", "dist/app.bundle.js"), encoding="utf-8"
            )
        print("  已复制: static/")
    else:
        print("  [警告] static/ 目录不存在，前端资源将缺失")

    ico_src = ROOT / "static" / "bulibuli.ico"
    if ico_src.exists():
        shutil.copy2(ico_src, package_dir / "bulibuli.ico")
        print("  已复制: bulibuli.ico")

    (package_dir / "data").mkdir(exist_ok=True)
    print("  已创建: data/")

    if platform_name in {"linux", "termux"}:
        installer_dir = "linux" if platform_name == "linux" else "termux"
        installer_src = ROOT / "deploy" / installer_dir / "install.sh"
        if installer_src.is_file():
            installer_dst = package_dir / "install.sh"
            shutil.copy2(installer_src, installer_dst)
            installer_dst.chmod(installer_dst.stat().st_mode | 0o111)
            print("  已复制: install.sh")
    write_package_manifest(package_dir, platform_name, target, variant)
    print(f"  已写入: {PACKAGE_MANIFEST_NAME}")
    validate_package_tree(package_dir, platform_name, variant)

    archive_format = "zip" if platform_name == "windows" else "gztar"
    archive_path = Path(
        shutil.make_archive(
            str(dist_dir / stem),
            archive_format,
            root_dir=str(dist_dir),
            base_dir=package_dir.name,
        )
    )
    checksum_path = write_checksum(archive_path)
    print(f"\n  {variant} 包目录: {package_dir}")
    print(f"  发布归档: {archive_path}")
    print(f"  校验文件: {checksum_path}")
    return package_dir, archive_path, checksum_path


def run_test(platform_name):
    """编译并直接启动程序（测试模式）"""
    check_cargo()
    print("[0/5] 清理残留的旧实例，避免端口被占用...")
    stop_existing_instances(platform_name)
    exe_path = build_release(platform_name)
    print("\n[测试模式] 启动程序...")
    print(f"  运行: {exe_path}")
    print("  启动后请查看控制台输出的 \"服务器监听于 http://...\" 行，")
    print("  在浏览器中打开该地址即可使用。\n")
    try:
        proc = subprocess.Popen([str(exe_path)])
        try:
            proc.wait()
        except KeyboardInterrupt:
            # Rust 进程已捕获 Ctrl+C 并正在优雅关闭，等待其完成
            print("\n[中断] 等待服务关闭...")
            try:
                proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                proc.terminate()
                proc.wait(timeout=5)
    except KeyboardInterrupt:
        print("\n[用户中断] 程序已退出")


def _quality_error(message):
    print(f"  [FAIL] {message}")
    return False


def check_resource_hashes():
    """Verify built-in resources against the hashes documented in resources/README.md."""
    manifest = ROOT / "resources" / "README.md"
    if not manifest.is_file():
        return _quality_error("resource hash manifest is missing")

    expected = {}
    in_code_block = False
    for raw_line in manifest.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line == "```":
            in_code_block = not in_code_block
            continue
        if not in_code_block:
            continue
        match = re.fullmatch(r"(\S+)\s+([0-9A-Fa-f]{64})", line)
        if match:
            expected[match.group(1)] = match.group(2).upper()

    if not expected:
        return _quality_error("resource hash manifest has no SHA-256 entries")

    ok = True
    for relative_path, wanted in expected.items():
        resource = ROOT / "resources" / relative_path
        if not resource.is_file():
            ok = _quality_error(f"resource is missing: resources/{relative_path}") and ok
            continue
        digest = hashlib.sha256()
        with resource.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        actual = digest.hexdigest().upper()
        if actual != wanted:
            ok = _quality_error(
                f"resource hash mismatch: resources/{relative_path} "
                f"(expected {wanted}, got {actual})"
            ) and ok
    return ok


def check_portable_bundle_contract(bundle):
    """Ensure the bundled frontend keeps the portable-directory capability contract."""
    bundle_text = bundle.read_text(encoding="utf-8")
    required_strings = (
        "can_open_directory",
        "open-history-directory",
        "path_display_mode",
    )
    ok = True
    for required in required_strings:
        if required not in bundle_text:
            ok = _quality_error(f"frontend bundle is missing contract field: {required}") and ok
    return ok


def check_windows_installer_encoding():
    """Windows PowerShell 5.1 needs a UTF-8 BOM to parse non-ASCII scripts."""
    installer = ROOT / "deploy" / "windows" / "install.ps1"
    if not installer.is_file():
        return _quality_error("Windows installer script is missing")
    if installer.read_bytes()[:3] != b"\xef\xbb\xbf":
        return _quality_error("Windows installer must be UTF-8 with BOM for PowerShell 5.1")
    return True


def run_quality_checks():
    """Run the mandatory formatting, lint, test and source-policy gates."""
    print("[check] running mandatory quality gates")
    commands = [
        (["cargo", "fmt", "--all", "--", "--check"], ROOT),
        (["cargo", "check", "--all-targets"], ROOT),
        (
            [
                "cargo",
                "clippy",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
            ROOT,
        ),
        (["cargo", "test", "--all-targets"], ROOT),
    ]
    # Node 原生测试与 Playwright 测试使用显式入口；不能由扩展名反推 runner。
    frontend_tests = sorted(
        path
        for path in (ROOT / "tests").glob("frontend_*.mjs")
        if not path.name.endswith(".spec.mjs")
    )
    if frontend_tests:
        commands.append(
            (
                ["node", "--test", *(str(path.relative_to(ROOT)) for path in frontend_tests)],
                ROOT,
            )
        )
    if sys.platform == "win32":
        npm_command = "npm.cmd"
        playwright_bin = ROOT / "static" / "js" / "node_modules" / ".bin" / "playwright.cmd"
    else:
        npm_command = "npm"
        playwright_bin = ROOT / "static" / "js" / "node_modules" / ".bin" / "playwright"
    if playwright_bin.is_file():
        commands.append(([npm_command, "run", "test:smoke"], ROOT / "static" / "js"))
    else:
        print("  [警告] 未找到 Playwright 可执行文件，跳过前端冒烟测试"
              "（先在 static/js 下执行 npm ci 后重跑可获得完整检查）")
    for command, cwd in commands:
        run(command, cwd=str(cwd))

    ok = True
    advisory_warnings = []

    def advisory(message):
        advisory_warnings.append(message)
        print(f"  [WARN][advisory] {message}")

    rust_files = sorted((ROOT / "src").rglob("*.rs"))
    for path in rust_files:
        line_count = len(path.read_text(encoding="utf-8").splitlines())
        if line_count > 500:
            advisory(f"{path.relative_to(ROOT)} has {line_count} lines (limit 500)")

    app_path = ROOT / "static" / "js" / "app.js"
    if app_path.exists() and len(app_path.read_text(encoding="utf-8").splitlines()) > 800:
        advisory("static/js/app.js exceeds 800 lines")

    first_party_js = [
        path
        for path in sorted((ROOT / "static" / "js").glob("*.js"))
        if not path.name.endswith(".min.js")
    ]
    for path in first_party_js:
        line_count = len(path.read_text(encoding="utf-8").splitlines())
        if line_count > 800:
            advisory(f"{path.relative_to(ROOT)} has {line_count} lines (limit 800)")
    import shutil
    if shutil.which("node") is None:
        print("  [错误] 未找到 node 可执行文件，无法执行 JavaScript 语法检查")
        raise SystemExit(1)
    for path in sorted((ROOT / "static" / "js").glob("*.js")):
        result = subprocess.run(
            ["node", "--input-type=module", "--check"],
            cwd=str(ROOT),
            input=path.read_text(encoding="utf-8"),
            text=True,
            encoding="utf-8",
        )
        if result.returncode != 0:
            print(f"  [错误] JavaScript 语法检查失败: {path.relative_to(ROOT)}")
            raise SystemExit(1)

    bundle = build_frontend_bundle()
    ok = check_resource_hashes() and ok
    ok = check_portable_bundle_contract(bundle) and ok
    ok = check_windows_installer_encoding() and ok

    rust_source = "\n".join(path.read_text(encoding="utf-8") for path in rust_files)
    js_source = "\n".join(path.read_text(encoding="utf-8") for path in first_party_js)
    html_source = "\n".join(
        (ROOT / "static" / name).read_text(encoding="utf-8")
        for name in ("index.html", "setup.html", "settings.html")
    )
    css_files = [ROOT / "static" / "css" / "style.css"] + sorted(
        (ROOT / "static" / "css").glob("*.css")
    )
    # Vendor CSS is third-party input and is checked by its own asset hash gate.
    css_files = [path for path in css_files if "static/css/lib" not in path.as_posix()]
    css_lines = [
        line
        for path in dict.fromkeys(css_files)
        for line in path.read_text(encoding="utf-8").splitlines()
    ]
    api_source = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted((ROOT / "src" / "api").rglob("*.rs"))
    )
    css_has_hardcoded_color = any(
        re.search(r"#[0-9a-fA-F]{3,8}\b|rgba?\(", line)
        and not re.match(r"\s*--[a-z0-9-]+\s*:", line)
        for line in css_lines
    )
    css_has_hardcoded_px = any(
        re.search(r"(?<![-\w])\d+(?:\.\d+)?px\b", line)
        and not re.match(r"\s*(?:--[a-z0-9-]+\s*:|@media\b)", line)
        for line in css_lines
    )
    policy_checks = [
        ("#[allow(dead_code)]", "#[allow(dead_code)]" not in rust_source),
        ("let _ = error swallowing", re.search(r"\blet\s+_\s*=", rust_source) is None),
        ("production console.log", "console.log" not in js_source),
        ("JavaScript inline styles", re.search(r"\.style\.|\sstyle\s*=", js_source) is None),
        ("HTML inline event handlers", re.search(r"\son[a-z]+\s*=", html_source, re.I) is None),
        ("HTML inline styles", re.search(r"\sstyle\s*=", html_source, re.I) is None),
        ("silent catch blocks", re.search(r"catch\s*(?:\([^)]*\))?\s*\{\s*\}", js_source) is None),
        (
            "legacy frontend success contract",
            re.search(r"\.success\b|[\"']success[\"']\s*:", js_source) is None,
        ),
        ("API layer database access", ".database()" not in api_source),
        ("CSS hardcoded colors outside tokens", not css_has_hardcoded_color),
        ("CSS hardcoded pixel values outside tokens", not css_has_hardcoded_px),
        (
            "CSS var() fallbacks",
            re.search(r"var\(\s*--[a-z0-9-]+\s*,", "\n".join(css_lines)) is None,
        ),
        (
            "numeric CSS color tokens",
            re.search(r"--color-token-[0-9]+\s*:", "\n".join(css_lines)) is None,
        ),
        (
            "CSP unsafe-inline",
            "unsafe-inline" not in rust_source,
        ),
    ]
    blocking_checks = [
        (
            "CSS empty var()",
            re.search(r"var\(\s*\)", "\n".join(css_lines)) is None,
        ),
    ]
    for label, passed in policy_checks:
        if not passed:
            advisory(label)
    for label, passed in blocking_checks:
        if not passed:
            ok = _quality_error(label) and ok

    for route in re.findall(r'\.route\(\s*"([^"]+)"', rust_source):
        static_route = re.sub(r"\{[^}]+\}", "param", route)
        if "_" in static_route:
            advisory(f"API route is not kebab-case: {route}")
    for dom_id in re.findall(r'\bid="([^"]+)"', html_source):
        if not re.fullmatch(r"[a-z][a-z0-9]*(?:-[a-z0-9]+)*", dom_id):
            advisory(f"DOM id is not kebab-case: {dom_id}")

    if not ok:
        raise SystemExit(1)
    print("[check] all mandatory gates passed")


def main():
    parser = argparse.ArgumentParser(description="补哩补哩 bulibuli 构建脚本")
    parser.add_argument("--check", action="store_true", help="运行全部规范门禁")
    parser.add_argument("--portable", action="store_true", help="构建当前平台完整便携版")
    parser.add_argument("--core", action="store_true", help="构建不含媒体运行时的轻量包")
    parser.add_argument("--skip-frontend", action="store_true", help="复用已有 static/dist/app.bundle.js")
    parser.add_argument("--skip-rust-build", action="store_true", help="复用已有 release 二进制")
    parser.add_argument(
        "--platform",
        choices=("auto", "windows", "linux", "macos", "termux"),
        default="auto",
        help="便携包平台（默认按当前系统判断）",
    )
    parser.add_argument("--target", help="可选 Rust target triple")
    args = parser.parse_args()

    if args.check:
        run_quality_checks()
        return

    try:
        platform_name = normalize_platform(args.platform)
    except ValueError as error:
        parser.error(str(error))

    print(f"=== {APP_DISPLAY_NAME} {APP_SLUG} v{APP_VERSION} 构建脚本 ===\n")

    if args.portable and args.core:
        parser.error("--portable 与 --core 不能同时使用")

    if not args.portable and not args.core:
        run_test(platform_name)
        return

    dist_dir = ROOT / "dist"
    dist_dir.mkdir(exist_ok=True)

    check_cargo()
    if args.skip_frontend:
        bundle = ROOT / "static" / "dist" / "app.bundle.js"
        if not bundle.is_file():
            parser.error("--skip-frontend 要求已有 static/dist/app.bundle.js")
    else:
        build_frontend_bundle()
    if args.skip_rust_build:
        exe_path = release_binary_path(platform_name, args.target)
        if not exe_path.is_file():
            parser.error(f"--skip-rust-build 要求已有 release 二进制：{exe_path}")
    else:
        exe_path = build_release(platform_name, args.target)
    variant = "portable" if args.portable else "core"
    package_dir, archive_path, checksum_path = assemble_package(
        exe_path, platform_name, args.target, variant
    )

    print("\n构建完成!")
    print(f"  {variant}: {package_dir}")
    print(f"  归档: {archive_path}")
    print(f"  SHA-256: {checksum_path}")
    print()


if __name__ == "__main__":
    main()
