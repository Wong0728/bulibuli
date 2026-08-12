#!/usr/bin/env bash
# 补哩补哩 bulibuli Linux 一键安装脚本。
#
# 远程安装：
#   curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/linux/install.sh | bash
#   # 固定版本（可复现）：
#   curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/linux/install.sh | BULIBULI_VERSION=v2.0.0-alpha.3 bash
#
# 本地发布包：
#   ./install.sh [install|run|service|unservice|status]
set -euo pipefail

APP_SLUG="bulibuli"
APP_VERSION="${BULIBULI_VERSION:-latest}"
if [ "${APP_VERSION}" != "latest" ]; then
    [[ "${APP_VERSION}" == v* ]] || APP_VERSION="v${APP_VERSION}"
fi
REPO="${BULIBULI_REPO:-Wong0728/bulibuli}"
BIN_NAME="${APP_SLUG}"
SERVICE_NAME="${APP_SLUG}"

SOURCE_FILE="${BASH_SOURCE[0]:-}"
if [ -f "${SOURCE_FILE}" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${SOURCE_FILE}")" && pwd)"
else
    SCRIPT_DIR="${PWD}"
fi

APP_DIR=""
BIN_PATH=""
MODE=""
REMOTE_BOOTSTRAP=0
TEMP_DIR=""
REMOTE_VARIANT="portable"

log()  { printf '\033[32m[bulibuli]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*"; }
die()  { printf '\033[31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

cleanup() {
    if [ -n "${TEMP_DIR}" ] && [ -d "${TEMP_DIR}" ]; then
        rm -rf -- "${TEMP_DIR}"
    fi
}
trap cleanup EXIT

download_file() {
    local url="$1"
    local destination="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fL --retry 3 --connect-timeout 15 "${url}" -o "${destination}"
    elif command -v wget >/dev/null 2>&1; then
        wget -O "${destination}" "${url}"
    else
        die "需要 curl 或 wget 才能下载 Release"
    fi
}

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
    local api_url="https://api.github.com/repos/${REPO}/releases?per_page=20"
    local tag
    tag="$(download_text "${api_url}" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    [[ "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || \
        die "无法从 GitHub Releases 解析最新版本；请用 BULIBULI_VERSION=vX.Y.Z 固定版本重试"
    printf '%s\n' "${tag}"
}

verify_checksum() {
    local archive="$1"
    local manifest="$2"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$(dirname "${archive}")" && sha256sum -c "$(basename "${manifest}")")
        return
    fi
    if command -v shasum >/dev/null 2>&1; then
        local expected actual
        expected="$(awk '{print $1}' "${manifest}")"
        actual="$(shasum -a 256 "${archive}" | awk '{print $1}')"
        [ "${expected}" = "${actual}" ] || die "SHA-256 校验失败"
        return
    fi
    die "系统没有 sha256sum 或 shasum，拒绝安装未校验的归档"
}

resolve_env_binary() {
    local variable="$1" name="$2" value candidate
    value="${!variable:-}"
    [ -n "${value}" ] || return 1
    if [ -x "${value}" ]; then
        printf '%s\n' "${value}"
        return 0
    fi
    candidate="${value}/${name}"
    [ -x "${candidate}" ] || return 1
    printf '%s\n' "${candidate}"
}

external_aria2_path() {
    resolve_env_binary ARIA2C_PATH aria2c || command -v aria2c 2>/dev/null || true
}

external_ffmpeg_path() {
    local value candidate
    for variable in FFMPEG_PATH FFMPEG FF_PATH FFMPEG_HOME FFMPEG_DIR; do
        value="${!variable:-}"
        [ -n "${value}" ] || continue
        if [ -x "${value}" ]; then
            printf '%s\n' "${value}"
            return 0
        fi
        candidate="${value}/ffmpeg"
        if [ -x "${candidate}" ]; then
            printf '%s\n' "${candidate}"
            return 0
        fi
    done
    command -v ffmpeg 2>/dev/null || true
}

external_ffprobe_path() {
    local ffmpeg_path="${1:-}" value candidate
    value="${FFPROBE_PATH:-}"
    if [ -x "${value}" ]; then
        printf '%s\n' "${value}"
        return 0
    fi
    if [ -n "${value}" ] && [ -x "${value}/ffprobe" ]; then
        printf '%s\n' "${value}/ffprobe"
        return 0
    fi
    if [ -n "${ffmpeg_path}" ]; then
        candidate="$(dirname "${ffmpeg_path}")/ffprobe"
        if [ -x "${candidate}" ]; then
            printf '%s\n' "${candidate}"
            return 0
        fi
    fi
    command -v ffprobe 2>/dev/null || true
}

runtime_available() {
    local aria2_path ffmpeg_path ffprobe_path
    aria2_path="$(external_aria2_path)"
    ffmpeg_path="$(external_ffmpeg_path)"
    ffprobe_path="$(external_ffprobe_path "${ffmpeg_path}")"
    [ -n "${aria2_path}" ] && [ -n "${ffmpeg_path}" ] || return 1
    "${aria2_path}" -v >/dev/null 2>&1 || return 1
    "${ffmpeg_path}" -version >/dev/null 2>&1 || return 1
}

missing_runtime_packages() {
    local missing=()
    [ -n "$(external_aria2_path)" ] || missing+=(aria2)
    if [ -z "$(external_ffmpeg_path)" ]; then
        missing+=(ffmpeg)
    fi
    printf '%s\n' "${missing[@]}"
}

install_system_deps_if_possible() {
    local missing=()
    mapfile -t missing < <(missing_runtime_packages)
    [ ${#missing[@]} -gt 0 ] || return 0
    local -a sudo_cmd=()
    [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1 && sudo_cmd=(sudo)
    if command -v apt-get >/dev/null 2>&1; then
        if ! "${sudo_cmd[@]}" apt-get update || ! "${sudo_cmd[@]}" apt-get install -y "${missing[@]}"; then return 1; fi
    elif command -v dnf >/dev/null 2>&1; then
        if ! "${sudo_cmd[@]}" dnf install -y "${missing[@]}"; then return 1; fi
    elif command -v yum >/dev/null 2>&1; then
        if ! "${sudo_cmd[@]}" yum install -y "${missing[@]}"; then return 1; fi
    elif command -v pacman >/dev/null 2>&1; then
        if ! "${sudo_cmd[@]}" pacman -Sy --noconfirm "${missing[@]}"; then return 1; fi
    elif command -v zypper >/dev/null 2>&1; then
        if ! "${sudo_cmd[@]}" zypper install -y "${missing[@]}"; then return 1; fi
    elif command -v apk >/dev/null 2>&1; then
        if ! "${sudo_cmd[@]}" apk add "${missing[@]}"; then return 1; fi
    else
        return 1
    fi
    runtime_available
}

choose_remote_variant() {
    if runtime_available; then
        REMOTE_VARIANT="core"
        log "检测到本机 aria2c 和 FFmpeg，将下载轻量 core 包"
        return
    fi
    if install_system_deps_if_possible; then
        REMOTE_VARIANT="core"
        log "系统依赖已就绪，将下载轻量 core 包"
    else
        REMOTE_VARIANT="portable"
        warn "本机缺少运行时或系统包管理器安装失败，将回退完整 portable 包"
    fi
}

download_release() {
    local arch archive_name archive_url checksum_url extracted
    if [ "${APP_VERSION}" = "latest" ]; then
        APP_VERSION="$(resolve_latest_version)"
        log "已解析最新 Release：${APP_VERSION}"
    fi
    case "$(uname -m)" in
        x86_64|amd64) arch="x86_64" ;;
        *) die "当前 Linux 架构暂不支持：$(uname -m)，目前提供 x86_64 包" ;;
    esac
    TEMP_DIR="$(mktemp -d)"
    archive_name="${APP_SLUG}-linux-${arch}-${REMOTE_VARIANT}-${APP_VERSION}.tar.gz"
    archive_url="https://github.com/${REPO}/releases/download/${APP_VERSION}/${archive_name}"
    checksum_url="${archive_url}.sha256"
    log "下载 ${archive_name}"
    if ! download_file "${archive_url}" "${TEMP_DIR}/${archive_name}"; then
        if [ "${REMOTE_VARIANT}" != "core" ]; then
            die "无法下载 ${archive_name}"
        fi
        REMOTE_VARIANT="portable"
        archive_name="${APP_SLUG}-linux-${arch}-portable-${APP_VERSION}.tar.gz"
        archive_url="https://github.com/${REPO}/releases/download/${APP_VERSION}/${archive_name}"
        checksum_url="${archive_url}.sha256"
        warn "该 Release 没有 core 包，回退下载完整 portable 包"
        download_file "${archive_url}" "${TEMP_DIR}/${archive_name}"
    fi
    download_file "${checksum_url}" "${TEMP_DIR}/${archive_name}.sha256"
    verify_checksum "${TEMP_DIR}/${archive_name}" "${TEMP_DIR}/${archive_name}.sha256"
    extracted="${TEMP_DIR}/${archive_name%.tar.gz}"
    mkdir -p "${TEMP_DIR}/unpacked"
    tar -xzf "${TEMP_DIR}/${archive_name}" -C "${TEMP_DIR}/unpacked"
    APP_DIR="${TEMP_DIR}/unpacked/$(basename "${extracted}")"
    [ -f "${APP_DIR}/${BIN_NAME}" ] || die "Release 归档缺少 ${BIN_NAME}"
    verify_runtime_checksums "${APP_DIR}"
    BIN_PATH="${APP_DIR}/${BIN_NAME}"
    MODE="release-package"
    REMOTE_BOOTSTRAP=1
}

verify_runtime_checksums() {
    local package_dir="$1"
    local resources_dir="${package_dir}/resources"
    [ -d "${resources_dir}" ] || return 0
    local manifest binary
    while IFS= read -r -d '' manifest; do
        binary="${manifest%.sha256}"
        [ -f "${binary}" ] || die "Release 校验清单缺少对应文件：${binary}"
        verify_checksum "${binary}" "${manifest}"
    done < <(find "${resources_dir}" -type f -name '*.sha256' -print0)
    if [ "${REMOTE_VARIANT}" = "portable" ]; then
        for name in aria2c ffmpeg; do
            [ -x "${resources_dir}/${name}" ] || die "Release 缺少可执行运行时：resources/${name}"
        done
    fi
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

    choose_remote_variant
    download_release
}

install_deps() {
    local bundled_aria2="${APP_DIR}/resources/aria2c"
    local bundled_ffmpeg="${APP_DIR}/resources/ffmpeg"
    if [ -f "${bundled_aria2}" ]; then chmod +x "${bundled_aria2}"; fi
    if [ -f "${bundled_ffmpeg}" ]; then chmod +x "${bundled_ffmpeg}"; fi
    if [ -x "${bundled_aria2}" ] && [ -x "${bundled_ffmpeg}" ]; then
        log "运行时依赖已就绪：优先使用 Release 包内置版本"
        return
    fi
    runtime_available || die "aria2c 或 FFmpeg 不可用；请安装系统依赖，或重新使用完整 portable 包"
    log "运行时依赖已就绪：使用环境变量路径或系统 PATH"
}

ensure_binary() {
    if [ -x "${BIN_PATH}" ]; then
        log "已找到二进制：${BIN_PATH}"
        return
    fi
    [ "${MODE}" = "release-package" ] && die "发布包缺少二进制 ${BIN_NAME}"
    command -v cargo >/dev/null 2>&1 || die "未找到 Rust 工具链，请先安装 rustup 后重试"
    log "开始编译 bulibuli（cargo build --release）..."
    (cd "${APP_DIR}" && cargo build --release)
    [ -x "${BIN_PATH}" ] || die "编译完成但未找到产物：${BIN_PATH}"
}

install_remote_package() {
    [ "${REMOTE_BOOTSTRAP}" -eq 1 ] || return
    local destination="${BULIBULI_INSTALL_DIR:-${XDG_DATA_HOME:-${HOME}/.local/share}/${APP_SLUG}}"
    mkdir -p "${destination}"
    cp -a "${APP_DIR}/." "${destination}/"
    APP_DIR="${destination}"
    BIN_PATH="${APP_DIR}/${BIN_NAME}"
    log "已安装到：${APP_DIR}"
}

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
    command -v systemctl >/dev/null 2>&1 || die "系统没有 systemd，请改用 './install.sh run'"
    service_paths
    mkdir -p "${UNIT_DIR}"
    local runtime_dir="${APP_DIR}"
    local service_identity=""
    local service_data=""
    local hardening=""
    local configured_data="${BULIBULI_DATA_DIR:-${BILI__DATA_DIR:-}}"
    if [ -n "${configured_data}" ] && [[ "${configured_data}" != /* ]]; then
        die "BULIBULI_DATA_DIR/BILI__DATA_DIR 必须是绝对路径"
    fi
    if [ "$(id -u)" -eq 0 ]; then
        local service_user="${APP_SLUG}"
        id "${service_user}" >/dev/null 2>&1 || useradd \
            --system --home-dir "/var/lib/${SERVICE_NAME}" \
            --create-home --shell /usr/sbin/nologin "${service_user}"
        runtime_dir="/opt/${SERVICE_NAME}"
        install -d -o root -g root -m 0755 "${runtime_dir}"
        install -m 0755 "${BIN_PATH}" "${runtime_dir}/${BIN_NAME}"
        install -d -o root -g root -m 0755 "${runtime_dir}/static"
        cp -a "${APP_DIR}/static/." "${runtime_dir}/static/"
        install -d -o root -g root -m 0755 "${runtime_dir}/resources"
        [ -d "${APP_DIR}/resources" ] && cp -a "${APP_DIR}/resources/." "${runtime_dir}/resources/"
        if [ -z "${configured_data}" ]; then
            configured_data="/var/lib/${SERVICE_NAME}"
        fi
        install -d -o "${service_user}" -g "${service_user}" -m 0700 "${configured_data}"
        chown "${service_user}:${service_user}" "${configured_data}"
        BIN_PATH="${runtime_dir}/${BIN_NAME}"
        service_identity="User=${service_user}
Group=${service_user}"
        service_data="Environment=BILI__DATA_DIR=${configured_data}
ReadWritePaths=${configured_data}"
        hardening="ProtectHome=true
PrivateDevices=true"
        case "${configured_data}" in
            /home/*|/root/*) hardening="PrivateDevices=true" ;;
        esac
    else
        if [ -z "${configured_data}" ]; then
            configured_data="${APP_DIR}/data"
        fi
        mkdir -p "${configured_data}"
        chmod 700 "${configured_data}"
        service_data="Environment=BILI__DATA_DIR=${configured_data}
ReadWritePaths=${configured_data}"
    fi

    cat > "${UNIT_FILE}" <<EOF
[Unit]
Description=补哩补哩 bulibuli（B站视频监控下载服务）
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
${service_identity}
WorkingDirectory=${runtime_dir}
ExecStart=${BIN_PATH}
Restart=always
RestartSec=5
KillMode=control-group
SendSIGKILL=yes
NoNewPrivileges=true
ProtectSystem=strict
PrivateTmp=true
${hardening}
${service_data}
TimeoutStopSec=30

[Install]
WantedBy=${WANTED_BY}
EOF
    "${SYSTEMCTL[@]}" daemon-reload
    "${SYSTEMCTL[@]}" enable --now "${SERVICE_NAME}"
    if [ "$(id -u)" -ne 0 ] && command -v loginctl >/dev/null 2>&1; then
        loginctl enable-linger "$(whoami)" || warn "开启 linger 失败，未登录时服务不会自启"
    fi
    log "服务已启动并设置为开机自启"
}

remove_service() {
    command -v systemctl >/dev/null 2>&1 || die "系统没有 systemd"
    service_paths
    "${SYSTEMCTL[@]}" disable --now "${SERVICE_NAME}" 2>/dev/null || true
    rm -f -- "${UNIT_FILE}"
    "${SYSTEMCTL[@]}" daemon-reload
    log "服务已停止并移除"
}

show_status() {
    command -v systemctl >/dev/null 2>&1 || die "系统没有 systemd"
    service_paths
    "${SYSTEMCTL[@]}" status "${SERVICE_NAME}" --no-pager || true
}

main() {
    local action="${1:-install}"
    case "${action}" in
        unservice) remove_service; return ;;
        status) show_status; return ;;
    esac

    detect_layout
    install_remote_package
    case "${action}" in
        install)
            install_deps
            ensure_binary
            log "安装完成。前台运行：./install.sh run；注册服务：./install.sh service"
            ;;
        run)
            install_deps
            ensure_binary
            log "前台启动（Ctrl+C 退出）"
            cd "${APP_DIR}"
            exec "${BIN_PATH}"
            ;;
        service)
            install_deps
            ensure_binary
            install_service
            ;;
        *)
            die "未知命令：${action}（可用：install / run / service / unservice / status）"
            ;;
    esac
}

main "$@"
