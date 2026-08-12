#!/data/data/com.termux/files/usr/bin/bash
# 补哩补哩 bulibuli Termux 一键部署脚本。
#
# 远程安装：
#   curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/termux/install.sh | bash
#   # 固定版本（可复现）：
#   curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/termux/install.sh | BULIBULI_VERSION=v2.0.0-alpha.3 bash
#
# 用法：
#   bash install.sh            安装依赖 + 本机编译
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

resolve_latest_version() {
    local tag
    tag="$(download_text "https://api.github.com/repos/${REPO}/releases?per_page=20" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    [[ "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || die "无法解析最新 Release 版本"
    printf '%s\n' "${tag}"
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
    if [ -f "${repo_root}/Cargo.toml" ] && [ -f "${repo_root}/static/index.html" ]; then
        APP_DIR="${repo_root}"
        BIN_PATH="${repo_root}/target/release/${BIN_NAME}"
        MODE="source"
        return
    fi

    APP_DIR="${REMOTE_SOURCE_DIR}"
    BIN_PATH="${APP_DIR}/target/release/${BIN_NAME}"
    MODE="remote-source"
}

install_deps() {
    log "安装 Termux 依赖（git、Rust、aria2、FFmpeg）..."
    pkg update -y
    pkg install -y git rust binutils aria2 ffmpeg
}

ensure_source() {
    [ "${MODE}" = "remote-source" ] || return
    if [ -f "${APP_DIR}/Cargo.toml" ]; then
        return
    fi
    if [ "${APP_VERSION}" = "latest" ]; then
        APP_VERSION="$(resolve_latest_version)"
        log "已解析最新 Release：${APP_VERSION}"
    fi
    rm -rf -- "${APP_DIR}"
    mkdir -p "$(dirname "${APP_DIR}")"
    log "下载 bulibuli ${APP_VERSION} 源码..."
    git clone --depth 1 --branch "${APP_VERSION}" \
        "https://github.com/${REPO}.git" "${APP_DIR}"
}

ensure_binary() {
    ensure_source
    if [ -x "${BIN_PATH}" ]; then
        log "已找到二进制：${BIN_PATH}"
        return
    fi
    command -v cargo >/dev/null 2>&1 || die "未找到 cargo，请先运行 bash install.sh"
    log "首次使用需在 Termux 本机编译一次..."
    (cd "${APP_DIR}" && cargo build --release)
    [ -x "${BIN_PATH}" ] || die "编译完成但未找到产物：${BIN_PATH}"
    log "编译完成：${BIN_PATH}"
}

is_running() {
    [ -f "${PID_FILE}" ] && kill -0 "$(cat "${PID_FILE}")" 2>/dev/null
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
            log "安装完成。后台启动：bash install.sh start；开机自启：bash install.sh boot"
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
