#!/data/data/com.termux/files/usr/bin/bash
# 补哩补哩 bulibuli Termux 一键部署脚本。
# 默认下载 GitHub Release 的 Termux/arm64 预编译包；需要源码构建时显式设置 BULIBULI_SOURCE_BUILD=1。
#
# 远程安装：
#   curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/termux/install.sh | bash
#   # 固定版本（可复现）：
#   curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/termux/install.sh | BULIBULI_VERSION=vX.Y.Z bash
#
# 用法：
#   bash install.sh            安装依赖 + 下载预编译包
#   bash install.sh start      后台启动（nohup + wake-lock）
#   bash install.sh stop       停止后台实例
#   bash install.sh run        前台运行
#   bash install.sh boot       配置 Termux:Boot 开机自启
#   bash install.sh unboot     移除开机自启
#   bash install.sh status     查看运行状态
set -euo pipefail

[ -n "${PREFIX:-}" ] || { printf '[error] 需要在 Termux 中运行\n' >&2; exit 1; }

APP_SLUG="bulibuli"
APP_VERSION="${BULIBULI_VERSION:-latest}"
if [ "${APP_VERSION}" != "latest" ]; then
    [[ "${APP_VERSION}" == v* ]] || APP_VERSION="v${APP_VERSION}"
fi
REPO="${BULIBULI_REPO:-Wong0728/bulibuli}"
BIN_NAME="${APP_SLUG}"
SCRIPT_SOURCE="${BASH_SOURCE[0]:-}"
if [ -f "${SCRIPT_SOURCE}" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${SCRIPT_SOURCE}")" && pwd)"
else
    SCRIPT_DIR="${PWD}"
fi

BOOT_DIR="${HOME}/.termux/boot"
BOOT_SCRIPT="${BOOT_DIR}/${APP_SLUG}.sh"
PID_FILE="${HOME}/.${APP_SLUG}.pid"
LOG_FILE="${HOME}/.${APP_SLUG}.nohup.log"
REMOTE_SOURCE_DIR="${PREFIX}/opt/${APP_SLUG}"
CACHE_DIR="${HOME}/.cache/${APP_SLUG}"
APP_DIR=""
BIN_PATH=""
MODE=""

log()  { printf '\033[32m[termux]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }
die()  { printf '\033[31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

download_text() {
    local url="$1"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 --connect-timeout 15 -H 'Accept: application/vnd.github+json' "${url}"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "${url}"
    else
        die "需要 curl 或 wget 才能查询 Release"
    fi
}

download_file() {
    local url="$1" destination="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fL --retry 3 --connect-timeout 15 "${url}" -o "${destination}"
    elif command -v wget >/dev/null 2>&1; then
        wget -O "${destination}" "${url}"
    else
        die "需要 curl 或 wget 才能下载 Release"
    fi
}

termux_architecture() {
    case "$(uname -m)" in
        aarch64|arm64) printf 'arm64\n' ;;
        *) die "当前 Termux 架构暂不支持：$(uname -m)，目前提供 arm64 预编译包" ;;
    esac
}

resolve_latest_version() {
    local tag=""
    tag="$(download_text "https://github.com/${REPO}/releases/latest/download/latest.json" 2>/dev/null | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1 || true)"
    if ! [[ "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
        tag="$(download_text "https://api.github.com/repos/${REPO}/releases?per_page=20" 2>/dev/null | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1 || true)"
    fi
    [[ "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || die "无法解析最新 Release 版本"
    printf '%s\n' "${tag}"
}

verify_checksum() {
    local archive="$1" manifest="$2"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$(dirname "${archive}")" && sha256sum -c "$(basename "${manifest}")")
        return
    fi
    if command -v shasum >/dev/null 2>&1; then
        local expected actual
        expected="$(awk '{print $1}' "${manifest}")"
        actual="$(shasum -a 256 "${archive}" | awk '{print $1}')"
        [ "${expected}" = "${actual}" ] || die "SHA-256 校验失败：${archive}"
        return
    fi
    die "系统没有 sha256sum 或 shasum，拒绝安装未校验的归档"
}

verify_package_manifest() {
    local package_dir="$1" expected_version="${2:-}"
    command -v python3 >/dev/null 2>&1 || die "需要 Python 校验 Termux 包清单"
    python3 - "${package_dir}" "${expected_version#v}" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1]).resolve()
expected = sys.argv[2]
manifest_path = root / "bulibuli.package.json"
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
if manifest.get("schema_version") != 1:
    raise SystemExit("unsupported package manifest")
if manifest.get("platform") != "termux" or manifest.get("architecture") != "arm64":
    raise SystemExit("package is not a Termux arm64 build")
if expected and manifest.get("app_version") != expected:
    raise SystemExit("package version mismatch")
for required in (root / "bulibuli", root / "install.sh", root / "static" / "index.html"):
    if not required.is_file():
        raise SystemExit(f"missing package file: {required.relative_to(root)}")
for entry in manifest.get("files", []):
    relative = pathlib.PurePosixPath(str(entry["path"]))
    if relative.is_absolute() or ".." in relative.parts:
        raise SystemExit("unsafe package manifest path")
    file_path = root.joinpath(*relative.parts).resolve()
    if root not in file_path.parents or not file_path.is_file():
        raise SystemExit("package manifest file missing")
    digest = hashlib.sha256(file_path.read_bytes()).hexdigest()
    if digest.lower() != str(entry.get("sha256", "")).lower():
        raise SystemExit(f"package file checksum mismatch: {relative}")
PY
}

detect_layout() {
    if [ -x "${SCRIPT_DIR}/${BIN_NAME}" ] && [ -f "${SCRIPT_DIR}/static/index.html" ]; then
        APP_DIR="${SCRIPT_DIR}"
        BIN_PATH="${SCRIPT_DIR}/${BIN_NAME}"
        MODE="release-package"
        return
    fi

    local repo_root
    repo_root="$(cd "${SCRIPT_DIR}/../.." 2>/dev/null && pwd || true)"
    if [ "${BULIBULI_SOURCE_BUILD:-0}" = "1" ] &&
        [ -f "${repo_root}/Cargo.toml" ] && [ -f "${repo_root}/static/index.html" ]; then
        APP_DIR="${repo_root}"
        BIN_PATH="${repo_root}/target/release/${BIN_NAME}"
        MODE="source"
        return
    fi

    APP_DIR="${REMOTE_SOURCE_DIR}"
    BIN_PATH="${APP_DIR}/${BIN_NAME}"
    if [ "${BULIBULI_SOURCE_BUILD:-0}" = "1" ]; then
        BIN_PATH="${APP_DIR}/target/release/${BIN_NAME}"
        MODE="remote-source"
    else
        MODE="remote-package"
    fi
}

install_deps() {
    log "安装 Termux 运行依赖（curl、Python、aria2、FFmpeg）..."
    pkg update -y
    pkg install -y curl python aria2 ffmpeg
}

install_source_deps() {
    log "安装源码构建依赖（git、Rust、binutils）..."
    pkg install -y git rust binutils
}

ensure_remote_package() {
    [ "${MODE}" = "remote-package" ] || return
    local arch package_name base_url archive checksum stage root child
    arch="$(termux_architecture)"
    [ "${APP_VERSION}" != "latest" ] || APP_VERSION="$(resolve_latest_version)"
    package_name="${APP_SLUG}-termux-${arch}-portable-${APP_VERSION}.tar.gz"
    base_url="https://github.com/${REPO}/releases/download/${APP_VERSION}"
    archive="${CACHE_DIR}/${package_name}"
    checksum="${archive}.sha256"

    if [ -x "${APP_DIR}/${BIN_NAME}" ] && [ -f "${APP_DIR}/bulibuli.package.json" ] && \
        verify_package_manifest "${APP_DIR}" "${APP_VERSION}" >/dev/null 2>&1; then
        BIN_PATH="${APP_DIR}/${BIN_NAME}"
        log "已找到 Termux 预编译包：${APP_DIR}"
        return
    fi

    mkdir -p "${CACHE_DIR}"
    log "下载 Termux 预编译包：${APP_VERSION}"
    download_file "${base_url}/${package_name}" "${archive}"
    download_file "${base_url}/${package_name}.sha256" "${checksum}"
    verify_checksum "${archive}" "${checksum}"

    stage="$(mktemp -d "${PREFIX}/tmp/${APP_SLUG}.XXXXXX")"
    tar -xzf "${archive}" -C "${stage}"
    root="$(find "${stage}" -mindepth 1 -maxdepth 1 -type d -print -quit)"
    [ -n "${root}" ] || die "Termux 归档目录缺失"
    verify_package_manifest "${root}" "${APP_VERSION}"
    mkdir -p "${APP_DIR}"
    for child in "${root}"/*; do
        [ "$(basename "${child}")" = "data" ] && continue
        cp -a "${child}" "${APP_DIR}/"
    done
    mkdir -p "${APP_DIR}/data"
    verify_package_manifest "${APP_DIR}" "${APP_VERSION}"
    rm -rf -- "${stage}"
    BIN_PATH="${APP_DIR}/${BIN_NAME}"
}

ensure_source() {
    [ "${MODE}" = "source" ] || [ "${MODE}" = "remote-source" ] || return
    [ "${MODE}" = "remote-source" ] || return
    if [ -f "${APP_DIR}/Cargo.toml" ]; then
        if [ "${APP_VERSION}" != "latest" ]; then
            git -C "${APP_DIR}" fetch --depth 1 origin "${APP_VERSION}" || die "无法获取指定版本 ${APP_VERSION}"
            git -C "${APP_DIR}" checkout --detach "${APP_VERSION}" || die "无法切换到指定版本 ${APP_VERSION}"
        fi
        return
    fi
    if [ "${APP_VERSION}" = "latest" ]; then
        APP_VERSION="$(resolve_latest_version)"
        log "已解析最新 Release：${APP_VERSION}"
    fi
    if [ -e "${APP_DIR}" ]; then
        die "源码目录已存在但不是 bulibuli 源码：${APP_DIR}（不会删除已有目录）"
    fi
    mkdir -p "$(dirname "${APP_DIR}")"
    log "下载 bulibuli ${APP_VERSION} 源码..."
    git clone --depth 1 --branch "${APP_VERSION}" \
        "https://github.com/${REPO}.git" "${APP_DIR}"
}

ensure_binary() {
    ensure_remote_package
    ensure_source
    if [ -x "${BIN_PATH}" ]; then
        log "已找到二进制：${BIN_PATH}"
        return
    fi
    install_source_deps
    command -v cargo >/dev/null 2>&1 || die "未找到 cargo，请先运行 bash install.sh"
    log "开始在 Termux 本机编译..."
    (cd "${APP_DIR}" && cargo build --release)
    [ -x "${BIN_PATH}" ] || die "编译完成但未找到产物：${BIN_PATH}"
    log "编译完成：${BIN_PATH}"
}

is_running() {
    local pid
    [ -f "${PID_FILE}" ] || return 1
    pid="$(cat "${PID_FILE}")"
    kill -0 "${pid}" 2>/dev/null || return 1
    [ -r "/proc/${pid}/cmdline" ] && tr '\0' ' ' <"/proc/${pid}/cmdline" | grep -F -- "${BIN_PATH}" >/dev/null
}

start_daemon() {
    if is_running; then
        log "已在运行（PID $(cat "${PID_FILE}")）"
        return
    fi
    command -v termux-wake-lock >/dev/null 2>&1 && termux-wake-lock || \
        warn "无 termux-wake-lock，休眠时下载可能中断"
    (cd "${APP_DIR}" && nohup "${BIN_PATH}" >"${LOG_FILE}" 2>&1 & echo $! >"${PID_FILE}")
    sleep 1
    is_running && log "已后台启动（PID $(cat "${PID_FILE}")），日志：${LOG_FILE}" || \
        die "启动失败，请查看日志：${LOG_FILE}"
}

stop_daemon() {
    if is_running; then
        local pid
        pid="$(cat "${PID_FILE}")"
        kill -TERM "${pid}"
        for _ in $(seq 1 30); do
            kill -0 "${pid}" 2>/dev/null || break
            sleep 1
        done
        if kill -0 "${pid}" 2>/dev/null; then
            warn "优雅关闭超时，强制终止"
            kill -9 "${pid}" || true
        fi
        rm -f -- "${PID_FILE}"
        log "已停止"
    else
        log "未在运行"
    fi
    command -v termux-wake-unlock >/dev/null 2>&1 && termux-wake-unlock || true
}

setup_boot() {
    mkdir -p "${BOOT_DIR}"
    cat > "${BOOT_SCRIPT}" <<EOF
#!/data/data/com.termux/files/usr/bin/bash
termux-wake-lock
cd "${APP_DIR}"
nohup "${BIN_PATH}" >"${LOG_FILE}" 2>&1 &
echo \$! >"${PID_FILE}"
EOF
    chmod +x "${BOOT_SCRIPT}"
    log "已写入 Termux:Boot 脚本：${BOOT_SCRIPT}"
    log "请安装并打开过 Termux:Boot 应用，重启手机后生效"
}

remove_boot() {
    rm -f -- "${BOOT_SCRIPT}"
    log "已移除开机自启脚本"
}

show_status() {
    if is_running; then
        log "运行中（PID $(cat "${PID_FILE}")）"
    else
        log "未在运行"
    fi
    [ -f "${BOOT_SCRIPT}" ] && log "开机自启：已配置" || log "开机自启：未配置"
}

main() {
    local action="${1:-install}"
    case "${action}" in
        stop) stop_daemon; return ;;
        status) show_status; return ;;
        unboot) remove_boot; return ;;
    esac

    detect_layout
    install_deps
    ensure_binary
    case "${action}" in
        install)
            log "安装完成。后台启动：bash \"${APP_DIR}/install.sh\" start；开机自启：bash \"${APP_DIR}/install.sh\" boot"
            ;;
        run)
            log "前台启动（Ctrl+C 退出）"
            cd "${APP_DIR}"
            exec "${BIN_PATH}"
            ;;
        start) start_daemon ;;
        boot) setup_boot ;;
        *) die "未知命令：${action}（可用：install / run / start / stop / boot / unboot / status）" ;;
    esac
}

main "$@"
