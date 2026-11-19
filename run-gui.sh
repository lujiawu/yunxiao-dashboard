#!/usr/bin/env bash
# 启动任务看板 GUI。非桌面会话的 shell 里也能用：自动补 Wayland 环境变量。
set -euo pipefail
cd "$(dirname "$0")"

runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export XDG_RUNTIME_DIR="$runtime"
# 值可能是 wayland-0 或 wayland-1，按实际 socket 取。
export WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-$(ls "$runtime" 2>/dev/null | grep -m1 '^wayland-[0-9]\+$' || true)}"

cargo build -p taskdash-gui
exec ./target/debug/taskdash-gui