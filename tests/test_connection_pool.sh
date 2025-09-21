#!/bin/bash

# 测试连接池功能的脚本

echo "=== AnyTLS 连接池测试脚本 ==="
echo

# 检查是否已经编译
if [ ! -f "target/debug/anytls-server" ]; then
    echo "编译 server..."
    cargo build --bin anytls-server
fi

if [ ! -f "target/debug/anytls-client" ]; then
    echo "编译 client..."
    cargo build --bin anytls-client
fi

echo "启动 server (带连接池)..."
echo "参数说明："
echo "  - 最大连接数: 100"
echo "  - 最大空闲时间: 300秒"
echo "  - 最小空闲连接: 5"
echo

# 启动 server
RUST_LOG=info ./target/debug/anytls-server \
    --listen 0.0.0.0:8443 \
    --password "test123" \
    --max-outbound-connections 100 \
    --max-idle-time-secs 300 \
    --min-idle-connections 5 &

SERVER_PID=$!
echo "Server PID: $SERVER_PID"

# 等待 server 启动
sleep 3

echo
echo "启动 client..."
echo "Client 将连接到多个网站来测试连接池效果"
echo

# 启动 client
RUST_LOG=info ./target/debug/anytls-client \
    --listen 127.0.0.1:1080 \
    --server 127.0.0.1:8443 \
    --password "test123" &

CLIENT_PID=$!
echo "Client PID: $CLIENT_PID"

# 等待 client 启动
sleep 3

echo
echo "=== 测试说明 ==="
echo "1. Server 现在支持外网连接池"
echo "2. 查看 server 日志中的连接池统计信息"
echo "3. 使用 SOCKS5 代理测试多个连接"
echo "4. 观察连接重用情况"
echo
echo "测试命令示例："
echo "  curl --socks5 127.0.0.1:1080 http://httpbin.org/get"
echo "  curl --socks5 127.0.0.1:1080 http://httpbin.org/ip"
echo "  curl --socks5 127.0.0.1:1080 https://www.google.com"
echo
echo "按 Ctrl+C 停止测试"

# 等待用户中断
trap "echo; echo '停止测试...'; kill $SERVER_PID $CLIENT_PID 2>/dev/null; exit 0" INT

wait
