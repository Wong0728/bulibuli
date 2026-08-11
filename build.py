#!/usr/bin/env python3
"""
BilibiliUIDBuildownloader 构建脚本

用法:
    python build.py              # 编译并直接启动程序（测试模式）
    python build.py --portable   # 构建便携版

产物:
    dist/BilibiliUIDBuild_portable/   便携版目录
"""

import argparse
import hashlib
import re
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent

APP_NAME = "BilibiliUIDBuild"
with (ROOT / "Cargo.toml").open("rb") as _cargo_file:
    APP_VERSION = tomllib.load(_cargo_file)["package"]["version"]
EXE_NAME = "bilibili-uid-buildownloader.exe"  # cargo 生成的 exe 名
PORTABLE_DIR_NAME = f"{APP_NAME}_portable"
PORTABLE_RESOURCE_FILES = ("aria2c.exe", "ffmpeg.exe", "README.md")
PORTABLE_RESOURCE_DIRS = ("geo",)


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
    导致长进程名（如 bilibili-uid-buildownloader.exe）匹配失败。
    """
    try:
        out = subprocess.run(
            ["tasklist", "/FI", f"IMAGENAME eq {exe_name}", "/FO", "CSV", "/NH"],
            capture_output=True,
            check=False,
        )
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


def stop_existing_instances():
    """关闭本程序之前残留的、且路径完全匹配的运行实例。"""
    if sys.platform != "win32":
        return

    expected_path = (ROOT / "target" / "release" / EXE_NAME).resolve()
    killed = _kill_processes_by_name(EXE_NAME, expected_path)
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


def build_release():
    """cargo build --release"""
    print("[2/5] 编译 Rust 项目 (release)...")
    run(["cargo", "build", "--release"], cwd=str(ROOT))
    exe_path = ROOT / "target" / "release" / EXE_NAME
    if not exe_path.exists():
        print(f"  [错误] 编译产物不存在: {exe_path}")
        sys.exit(1)
    print(f"  编译完成: {exe_path}")
    return exe_path


def assemble_portable(exe_path):
    """组装便携版目录"""
    print("[3/5] 组装便携版目录...")
    dist_dir = ROOT / "dist"
    portable_dir = dist_dir / PORTABLE_DIR_NAME

    if portable_dir.exists():
        shutil.rmtree(portable_dir)
    portable_dir.mkdir(parents=True)

    shutil.copy2(exe_path, portable_dir / f"{APP_NAME}.exe")
    print(f"  已复制: {APP_NAME}.exe")

    resources_src = ROOT / "resources"
    if resources_src.exists():
        resources_dst = portable_dir / "resources"
        resources_dst.mkdir()
        copied_resources = []
        for name in PORTABLE_RESOURCE_FILES:
            source = resources_src / name
            if source.is_file():
                shutil.copy2(source, resources_dst / name)
                copied_resources.append(name)
        for name in PORTABLE_RESOURCE_DIRS:
            source = resources_src / name
            if source.is_dir():
                shutil.copytree(source, resources_dst / name)
                copied_resources.append(f"{name}/")
        print(f"  已复制: resources/ ({', '.join(copied_resources) or '空'})")
    else:
        print("  [警告] resources/ 目录不存在，aria2c/ffmpeg 将缺失")

    static_src = ROOT / "static"
    if static_src.exists():
        shutil.copytree(static_src, portable_dir / "static")
        portable_index = portable_dir / "static" / "index.html"
        if (portable_dir / "static" / "dist" / "app.bundle.js").is_file() and portable_index.is_file():
            index_text = portable_index.read_text(encoding="utf-8")
            portable_index.write_text(
                index_text.replace("js/app.js", "dist/app.bundle.js"), encoding="utf-8"
            )
        print("  已复制: static/")
    else:
        print("  [警告] static/ 目录不存在，前端资源将缺失")

    ico_src = ROOT / "static" / "bilibili.ico"
    if ico_src.exists():
        shutil.copy2(ico_src, portable_dir / "bilibili.ico")
        print("  已复制: bilibili.ico")

    (portable_dir / "data").mkdir(exist_ok=True)
    print("  已创建: data/")

    print(f"\n  便携版目录: {portable_dir}")
    return portable_dir


def run_test():
    """编译并直接启动程序（测试模式）"""
    check_cargo()
    print("[0/5] 清理残留的旧实例，避免端口被占用...")
    stop_existing_instances()
    exe_path = build_release()
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


def run_quality_checks():
    """Run the mandatory formatting, lint, test and source-policy gates."""
    print("[check] running mandatory quality gates")
    commands = [
        ["cargo", "fmt", "--all", "--", "--check"],
        ["cargo", "check", "--all-targets"],
        [
            "cargo",
            "clippy",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        ["cargo", "test", "--all-targets"],
    ]
    # Playwright specs use the same .mjs suffix but are executed by its runner;
    # keep them out of the dependency-free Node contract test command.
    frontend_tests = sorted(
        path for path in (ROOT / "tests").glob("*.mjs") if not path.name.endswith(".spec.mjs")
    )
    if frontend_tests:
        commands.append(
            ["node", "--test", *(str(path.relative_to(ROOT)) for path in frontend_tests)]
        )
    for command in commands:
        run(command, cwd=str(ROOT))

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

    rust_source = "\n".join(path.read_text(encoding="utf-8") for path in rust_files)
    js_source = "\n".join(path.read_text(encoding="utf-8") for path in first_party_js)
    html_source = "\n".join(
        (ROOT / "static" / name).read_text(encoding="utf-8")
        for name in ("index.html", "setup.html", "settings.html")
    )
    css_files = [ROOT / "static" / "css" / "style.css"] + sorted(
        (ROOT / "static" / "css").glob("*.css")
    )
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
    parser = argparse.ArgumentParser(description="BilibiliUIDBuildownloader 构建脚本")
    parser.add_argument("--check", action="store_true", help="运行全部规范门禁")
    parser.add_argument("--portable", action="store_true", help="构建便携版")
    args = parser.parse_args()

    if args.check:
        run_quality_checks()
        return

    print(f"=== {APP_NAME} v{APP_VERSION} 构建脚本 ===\n")

    if not args.portable:
        run_test()
        return

    dist_dir = ROOT / "dist"
    dist_dir.mkdir(exist_ok=True)

    check_cargo()
    build_frontend_bundle()
    exe_path = build_release()
    assemble_portable(exe_path)

    print("\n构建完成!")
    print(f"  便携版: {dist_dir / PORTABLE_DIR_NAME}")
    print()


if __name__ == "__main__":
    main()
