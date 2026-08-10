#!/data/data/com.termux/files/usr/bin/bash
# BilibiliUIDBuildownloader Termux 一键部署脚本
#
# 用法:
#   bash install.sh            安装依赖 + 编译（Termux 无预编译产物，需本机编译一次）
#   bash install.sh start      后台启动（nohup + wake-lock）
#   bash install.sh stop       停止后台实例
#   bash install.sh run        前台运行（Ctrl+C 退出）
#   bash install.sh boot       配置开机自启（需安装 Termux:Boot 应用）
#   bash install.sh unboot     移除开机自启
#   bash install.sh status     查看运行状态
#
# 说明:
#   - 开机自启依赖 Termux:Boot 应用（F-Droid 可下载），安装后需手动打开过一次。
#   - 后台运行时持有 termux-wake-lock，避免 Android 休眠中断下载。
set -euo pipefail

BIN_NAME="bilibili-uid-buildownloader"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BOOT_DIR="${HOME}/.termux/boot"
BOOT_SCRIPT="${BOOT_DIR}/bilibili-downloader.sh"
PID_FILE="${HOME}/.bilibili-downloader.pid"
LOG_FILE="${HOME}/.bilibili-downloader.nohup.log"

log()  { printf '\033[32m[termux]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }
die()  { printf '\033[31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

detect_layout() {
    if [ -x "${SCRIPT_DIR}/${BIN_NAME}" ] && [ -f "${SCRIPT_DIR}/static/index.html" ]; then
        APP_DIR="${SCRIPT_DIR}"
        BIN_PATH="${SCRIPT_DIR}/${BIN_NAME}"
        return
    fi
    local repo_root
    repo_root="$(cd "${SCRIPT_DIR}/../.." && pwd)"
    if [ -f "${repo_root}/Cargo.toml" ] && [ -f "${repo_root}/static/index.html" ]; then
        APP_DIR="${repo_root}"
        BIN_PATH="${repo_root}/target/release/${BIN_NAME}"
        return
    fi
    die "无法定位应用目录（需在仓库 deploy/termux/ 下或与二进制同目录运行）"
}

install_deps() {
    log "安装运行时依赖 (aria2 ffmpeg)..."
    pkg install -y aria2 ffmpeg
}

ensure_binary() {
    if [ -x "${BIN_PATH}" ]; then
        log "已找到二进制: ${BIN_PATH}"
        return
    fi
    log "首次使用需在 Termux 本机编译一次（约需数分钟）..."
    command -v cargo >/dev/null 2>&1 || { log "安装 Rust 工具链..."; pkg install -y rust binutils; }
    (cd "${APP_DIR}" && cargo build --release)
    [ -x "${BIN_PATH}" ] || die "编译完成但未找到产物: ${BIN_PATH}"
    log "编译完成: ${BIN_PATH}"
}

is_running() {
    [ -f "${PID_FILE}" ] && kill -0 "$(cat "${PID_FILE}")" 2>/dev/null
}

start_daemon() {
    if is_running; then
        log "已在运行 (PID $(cat "${PID_FILE}"))"
        return
    fi
    command -v termux-wake-lock >/dev/null 2>&1 && termux-wake-lock || warn "无 termux-wake-lock，休眠时下载可能中断"
    (cd "${APP_DIR}" && nohup "${BIN_PATH}" >"${LOG_FILE}" 2>&1 & echo $! >"${PID_FILE}")
    sleep 1
    if is_running; then
        log "已后台启动 (PID $(cat "${PID_FILE}"))，Web 地址请以程序输出为准"
        log "日志: ${LOG_FILE}"
    else
        die "启动失败，请查看日志: ${LOG_FILE}"
    fi
}

stop_daemon() {
    if is_running; then
        local pid
        pid="$(cat "${PID_FILE}")"
        # SIGTERM 触发优雅关闭：保存 aria2 会话并关闭数据库
        kill -TERM "${pid}"
        for _ in $(seq 1 30); do
            kill -0 "${pid}" 2>/dev/null || break
            sleep 1
        done
        kill -0 "${pid}" 2>/dev/null && { warn "优雅关闭超时，强制终止"; kill -9 "${pid}" || true; }
        rm -f "${PID_FILE}"
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
# Termux:Boot 开机自启 BilibiliUIDBuildownloader
termux-wake-lock
cd "${APP_DIR}"
nohup "${BIN_PATH}" >"${LOG_FILE}" 2>&1 &
echo \$! >"${PID_FILE}"
EOF
    chmod +x "${BOOT_SCRIPT}"
    log "已写入开机自启脚本: ${BOOT_SCRIPT}"
    log "请确认已安装并打开过一次 Termux:Boot 应用（F-Droid 下载），重启手机后生效"
}

remove_boot() {
    rm -f "${BOOT_SCRIPT}"
    log "已移除开机自启脚本"
}

show_status() {
    if is_running; then
        log "运行中 (PID $(cat "${PID_FILE}"))，Web 地址请以程序输出为准"
    else
        log "未在运行"
    fi
    [ -f "${BOOT_SCRIPT}" ] && log "开机自启: 已配置 (${BOOT_SCRIPT})" || log "开机自启: 未配置"
}

main() {
    local action="${1:-install}"
    detect_layout
    case "${action}" in
        install)
            install_deps
            ensure_binary
            log "安装完成。后台启动: bash install.sh start    开机自启: bash install.sh boot"
            ;;
        run)
            install_deps
            ensure_binary
            log "前台启动 (Ctrl+C 退出)，Web 地址请以程序输出为准"
            cd "${APP_DIR}"
            exec "${BIN_PATH}"
            ;;
        start)  ensure_binary; start_daemon ;;
        stop)   stop_daemon ;;
        boot)   ensure_binary; setup_boot ;;
        unboot) remove_boot ;;
        status) show_status ;;
        *) die "未知命令: ${action}（可用: install / run / start / stop / boot / unboot / status）" ;;
    esac
}

main "$@"
