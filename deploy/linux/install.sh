#!/usr/bin/env bash
# BilibiliUIDBuildownloader Linux 一键部署脚本（命令行/服务器场景）
#
# 用法:
#   ./install.sh            安装依赖 + 准备二进制（预编译优先，缺失才编译）
#   ./install.sh run        前台运行（Ctrl+C 退出）
#   ./install.sh service    以上全部 + 注册 systemd 服务并设置开机自启
#   ./install.sh unservice  停止并移除 systemd 服务
#   ./install.sh status     查看服务状态
#
# 说明:
#   - 发布包场景: 本脚本与主程序二进制、static/ 同目录，直接可用，无需编译。
#   - 源码场景:   在仓库 deploy/linux/ 下运行，自动 cargo build --release。
#   - root 运行注册系统级服务；普通用户注册 user 服务并开启 linger 保证开机自启。
set -euo pipefail

BIN_NAME="bilibili-uid-buildownloader"
SERVICE_NAME="bilibili-downloader"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

log()  { printf '\033[32m[install]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }
die()  { printf '\033[31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

# ---------- 定位应用根目录与二进制 ----------
# 优先级: 脚本同目录的发布包二进制 > 仓库源码构建产物
detect_layout() {
    if [ -x "${SCRIPT_DIR}/${BIN_NAME}" ] && [ -f "${SCRIPT_DIR}/static/index.html" ]; then
        APP_DIR="${SCRIPT_DIR}"
        BIN_PATH="${SCRIPT_DIR}/${BIN_NAME}"
        MODE="release-package"
        return
    fi
    # 源码场景: deploy/linux/ 向上两级是仓库根
    local repo_root
    repo_root="$(cd "${SCRIPT_DIR}/../.." && pwd)"
    if [ -f "${repo_root}/Cargo.toml" ] && [ -f "${repo_root}/static/index.html" ]; then
        APP_DIR="${repo_root}"
        BIN_PATH="${repo_root}/target/release/${BIN_NAME}"
        MODE="source"
        return
    fi
    die "无法定位应用: 既没有同目录的 ${BIN_NAME}+static/, 也没有仓库源码结构"
}

# ---------- 安装运行时依赖 aria2 / ffmpeg ----------
install_deps() {
    local missing=()
    command -v aria2c >/dev/null 2>&1 || missing+=(aria2)
    command -v ffmpeg >/dev/null 2>&1 || missing+=(ffmpeg)
    if [ ${#missing[@]} -eq 0 ]; then
        log "运行时依赖已就绪: aria2c、ffmpeg"
        return
    fi
    log "需要安装: ${missing[*]}"
    local sudo_cmd=""
    [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1 && sudo_cmd="sudo"
    if command -v apt-get >/dev/null 2>&1; then
        ${sudo_cmd} apt-get update -y && ${sudo_cmd} apt-get install -y "${missing[@]}"
    elif command -v dnf >/dev/null 2>&1; then
        ${sudo_cmd} dnf install -y "${missing[@]}"
    elif command -v yum >/dev/null 2>&1; then
        ${sudo_cmd} yum install -y "${missing[@]}"
    elif command -v pacman >/dev/null 2>&1; then
        ${sudo_cmd} pacman -Sy --noconfirm "${missing[@]}"
    elif command -v zypper >/dev/null 2>&1; then
        ${sudo_cmd} zypper install -y "${missing[@]}"
    elif command -v apk >/dev/null 2>&1; then
        ${sudo_cmd} apk add "${missing[@]}"
    else
        warn "未识别的包管理器，请手动安装: ${missing[*]}"
        warn "程序仍可启动，但下载(aria2c)/合并(ffmpeg)功能不可用"
    fi
}

# ---------- 准备二进制（预编译优先，缺失才编译） ----------
ensure_binary() {
    if [ -x "${BIN_PATH}" ]; then
        log "已找到二进制: ${BIN_PATH}"
        return
    fi
    [ "${MODE}" = "release-package" ] && die "发布包缺少二进制 ${BIN_NAME}"
    command -v cargo >/dev/null 2>&1 || die "未找到预编译二进制且未安装 Rust 工具链。安装: https://rustup.rs 后重试"
    log "未找到预编译二进制，开始编译 (cargo build --release)..."
    (cd "${APP_DIR}" && cargo build --release)
    [ -x "${BIN_PATH}" ] || die "编译完成但未找到产物: ${BIN_PATH}"
    log "编译完成: ${BIN_PATH}"
}

# ---------- systemd 服务 ----------
service_paths() {
    if [ "$(id -u)" -eq 0 ]; then
        UNIT_DIR="/etc/systemd/system"
        SYSTEMCTL=(systemctl)
        WANTED_BY="multi-user.target"
    else
        UNIT_DIR="${HOME}/.config/systemd/user"
        SYSTEMCTL=(systemctl --user)
        WANTED_BY="default.target"
    fi
    UNIT_FILE="${UNIT_DIR}/${SERVICE_NAME}.service"
}

install_service() {
    command -v systemctl >/dev/null 2>&1 || die "系统没有 systemd，请改用 './install.sh run' 配合 nohup/tmux 运行"
    service_paths
    mkdir -p "${UNIT_DIR}"
    local runtime_dir="${APP_DIR}"
    local service_identity=""
    local service_data=""
    local hardening=""
    if [ "$(id -u)" -eq 0 ]; then
        local service_user="bilibili-downloader"
        id "${service_user}" >/dev/null 2>&1 || useradd \
            --system --home-dir "/var/lib/${SERVICE_NAME}" \
            --create-home --shell /usr/sbin/nologin "${service_user}"
        runtime_dir="/opt/${SERVICE_NAME}"
        install -d -o root -g root -m 0755 "${runtime_dir}"
        install -m 0755 "${BIN_PATH}" "${runtime_dir}/${BIN_NAME}"
        install -d -o root -g root -m 0755 "${runtime_dir}/static"
        cp -a "${APP_DIR}/static/." "${runtime_dir}/static/"
        chown -R root:root "${runtime_dir}/static"
        # 只发布程序实际使用的运行时资源，避免把源码目录中的未知文件带入服务目录。
        install -d -o root -g root -m 0755 "${runtime_dir}/resources"
        for resource in aria2c.exe ffmpeg.exe README.md; do
            if [ -f "${APP_DIR}/resources/${resource}" ]; then
                install -m 0644 "${APP_DIR}/resources/${resource}" "${runtime_dir}/resources/${resource}"
            fi
        done
        if [ -d "${APP_DIR}/resources/geo" ]; then
            cp -a "${APP_DIR}/resources/geo" "${runtime_dir}/resources/"
        fi
        install -d -o "${service_user}" -g "${service_user}" -m 0700 "/var/lib/${SERVICE_NAME}"
        BIN_PATH="${runtime_dir}/${BIN_NAME}"
        service_identity="User=${service_user}
Group=${service_user}"
        service_data="Environment=BILI__DATA_DIR=/var/lib/${SERVICE_NAME}
ReadWritePaths=/var/lib/${SERVICE_NAME}"
        hardening="ProtectHome=true
PrivateDevices=true"
    else
        # 非 root 的 user 服务：ProtectSystem=strict 下需显式放开应用 data 目录，
        # 否则程序无法写入数据库/下载文件（默认 WorkingDirectory/data）。
        service_data="ReadWritePaths=${APP_DIR}/data"
    fi
    cat > "${UNIT_FILE}" <<EOF
[Unit]
Description=BilibiliUIDBuildownloader (B站视频监控下载服务)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
${service_identity}
WorkingDirectory=${runtime_dir}
ExecStart=${BIN_PATH}
Restart=always
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
PrivateTmp=true
${hardening}
${service_data}
# stop 时先发 SIGTERM，程序会保存 aria2 会话并关闭数据库，最多等 30 秒
TimeoutStopSec=30
# 如需改监听端口等，取消注释（BILI__ 前缀环境变量即程序配置）
# Environment=BILI__PORT=5000
# Environment=BILI__HOST=0.0.0.0

[Install]
WantedBy=${WANTED_BY}
EOF
    log "已写入服务文件: ${UNIT_FILE}"
    "${SYSTEMCTL[@]}" daemon-reload
    "${SYSTEMCTL[@]}" enable --now "${SERVICE_NAME}"
    if [ "$(id -u)" -ne 0 ]; then
        # 非 root 的 user 服务默认登录才启动，开启 linger 才能真正开机自启
        if command -v loginctl >/dev/null 2>&1; then
            loginctl enable-linger "$(whoami)" || warn "开启 linger 失败，未登录时服务不会自启"
        fi
    fi
    log "服务已启动并设置为开机自启"
    log "查看状态: ./install.sh status    查看日志: journalctl $([ "$(id -u)" -ne 0 ] && echo --user) -u ${SERVICE_NAME} -f"
    log "Web 地址请以程序启动时输出的实际监听地址为准"
}

remove_service() {
    command -v systemctl >/dev/null 2>&1 || die "系统没有 systemd"
    service_paths
    "${SYSTEMCTL[@]}" disable --now "${SERVICE_NAME}" 2>/dev/null || true
    rm -f "${UNIT_FILE}"
    "${SYSTEMCTL[@]}" daemon-reload
    log "服务已停止并移除"
}

show_status() {
    command -v systemctl >/dev/null 2>&1 || die "系统没有 systemd"
    service_paths
    "${SYSTEMCTL[@]}" status "${SERVICE_NAME}" --no-pager || true
}

# ---------- 主流程 ----------
main() {
    local action="${1:-install}"
    detect_layout
    log "部署模式: ${MODE}  应用目录: ${APP_DIR}"
    case "${action}" in
        install)
            install_deps
            ensure_binary
            log "安装完成。前台运行: ./install.sh run    注册服务并自启: ./install.sh service"
            ;;
        run)
            install_deps
            ensure_binary
            log "前台启动 (Ctrl+C 退出)，Web 地址请以程序输出为准"
            exec "${BIN_PATH}"
            ;;
        service)
            install_deps
            ensure_binary
            install_service
            ;;
        unservice)
            remove_service
            ;;
        status)
            show_status
            ;;
        *)
            die "未知命令: ${action}（可用: install / run / service / unservice / status）"
            ;;
    esac
}

main "$@"
