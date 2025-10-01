#!/bin/bash

# AnyTLS 交叉测试
# 使用 anytls-go、anytls-rs 互为对端

echo "=== AnyTLS 交叉测试 ==="
echo ""

echo "=== 提前创建符号连接 ==="
echo "mkdir ~/.bin/ && export PATH=~/.bin:$PATH"
echo "ln -sf ~/xxx/anytls-go/cmd/client/client ~/.bin/client"
echo "ln -sf ~/xxx/anytls-go/cmd/server/server ~/.bin/server"
echo "ln -sf ~/xxx/anytls-rs/target/debug/anytls-client ~/.bin/anytls-client"
echo "ln -sf ~/xxx/anytls-rs/target/debug/anytls-server ~/.bin/anytls-server"

echo "=== 停止现有进程 ==="
pkill -f "server.*8443" 2>/dev/null
pkill -f "anytls-client" 2>/dev/null
sleep 1

echo "=== 启动 anytls-go 服务器 ==="
LOG_LEVEL=debug server -l 0.0.0.0:8443 -p password &
echo "服务器 PID: $!"

sleep 1

echo "=== 启动 anytls-rs 客户端 ==="
RUST_LOG=debug anytls-client -l 0.0.0.0:1080 -s 127.0.0.1:8443 -p password &
echo "客户端 PID: $!"

sleep 1

echo "=== 测试 服务 连接 ==="
curl --connect-timeout 5 --max-time 5 -w "time_total:  %{time_total}\n" -x socks5h://127.0.0.1:1080 -I https://www.jd.com/

echo "=== 清理进程 ==="
pkill -f "server.*8443" 2>/dev/null
pkill -f "anytls-client" 2>/dev/null