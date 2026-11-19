#!/bin/sh
# Builds wordropbot against an older glibc than the server's and replaces the
# running binary. Server layout: README, section "Установка на сервер".
set -eu

host=${DEPLOY_HOST:-root@cyprusswing.dance}
user=${DEPLOY_USER:-wordropbot}
cd "$(dirname "$0")/.."
out="$PWD/target/deploy"
mkdir -p "$out"

docker run --rm \
    -v "$PWD:/src:ro" \
    -v wordropbot-cargo:/usr/local/cargo/registry \
    -v wordropbot-rustup:/usr/local/rustup \
    -v wordropbot-target:/target \
    -v "$out:/out" \
    -w /src \
    rust:1-bookworm \
    sh -c "cargo build --release --locked --target-dir /target \
        && install -m 755 -o $(id -u) -g $(id -g) /target/release/wordropbot /out/wordropbot"

scp "$out/wordropbot" "$host:/home/$user/bin/wordropbot.new"
ssh "$host" "chown $user: /home/$user/bin/wordropbot.new \
    && mv /home/$user/bin/wordropbot.new /home/$user/bin/wordropbot \
    && systemctl --user -M $user@ restart wordropbot \
    && systemctl --user -M $user@ --no-pager status wordropbot"
