#!/bin/sh
# 由 systemd 定时器调用：ghcr.io 上有新镜像时更新容器。
# 浏览器正在使用（签到、登录窗口或等待人工验证）时跳过，下一轮再试，避免打断。
set -eu

cd "${DEPLOY_DIR:-/opt/nodeseek-checkin}"

if docker top nodeseek-checkin 2>/dev/null | grep -q '/chromium'; then
    echo "浏览器正在使用，跳过本次更新"
    exit 0
fi

docker compose pull --quiet
# 镜像没有变化时不会重建容器
docker compose up -d
docker image prune -f >/dev/null
