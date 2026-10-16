#!/bin/sh
set -e

# 以 root 启动时修正数据目录权限，然后降权为 app 用户运行
if [ "$(id -u)" = 0 ]; then
    mkdir -p "$DATA_DIR"
    [ "$(stat -c %u "$DATA_DIR")" = 1000 ] || chown -R app:app "$DATA_DIR"
    export HOME=/home/app
    exec setpriv --reuid=app --regid=app --init-groups "$0" "$@"
fi

rm -f /tmp/.X99-lock /tmp/.X11-unix/X99
Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 &
for _ in $(seq 50); do
    [ -e /tmp/.X11-unix/X99 ] && break
    sleep 0.1
done
x11vnc -display "$DISPLAY" -localhost -forever -shared -nopw -quiet -rfbport 5900 >/dev/null 2>&1 &

exec nodeseek-checkin
