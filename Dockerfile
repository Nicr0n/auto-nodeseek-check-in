FROM rust:1-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# 只取 noVNC 的静态文件，不安装它的 Python 依赖
FROM debian:trixie-slim AS novnc
RUN apt-get update \
    && cd /tmp && apt-get download novnc \
    && dpkg -x novnc_*.deb /tmp/novnc \
    && cp -rL /tmp/novnc/usr/share/novnc /opt/novnc

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        chromium xvfb x11vnc xdotool fonts-noto-cjk tzdata tini ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 app \
    && mkdir -p /tmp/.X11-unix && chmod 1777 /tmp/.X11-unix
COPY --from=novnc /opt/novnc /opt/novnc
COPY --from=build /src/target/release/auto-nodeseek-check-in /usr/local/bin/nodeseek-checkin
COPY docker/entrypoint.sh /entrypoint.sh

ENV DATA_DIR=/data \
    DISPLAY=:99 \
    CHROME_BIN=chromium \
    NOVNC_DIR=/opt/novnc \
    TZ=Asia/Shanghai
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["tini", "--", "/entrypoint.sh"]
