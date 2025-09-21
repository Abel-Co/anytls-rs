#!/bin/bash

# 连接池性能测试脚本

echo "=== AnyTLS 连接池性能测试 ==="
echo

# 编译项目
echo "编译项目..."
cargo build --release --bin anytls-server
cargo build --release --bin anytls-client-optimized

echo "启动 server (Release 版本)..."
echo "连接池配置："
echo "  - 最大连接数: 50"
echo "  - 最大空闲时间: 60秒"
echo "  - 最小空闲连接: 3"
echo

# 启动 server
RUST_LOG=info cargo run --bin anytls-server --release -- \
    --listen 0.0.0.0:8443 \
    --password "test123" \
    --max-outbound-connections 50 \
    --max-idle-time-secs 60 \
    --min-idle-connections 3 &

SERVER_PID=$!
echo "Server PID: $SERVER_PID"

echo
echo "启动 client (Release 版本)..."
RUST_LOG=warn cargo run --bin anytls-client-optimized --release -- \
    --listen 127.0.0.1:1080 \
    --server 127.0.0.1:8443 \
    --password "test123" &

CLIENT_PID=$!
echo "Client PID: $CLIENT_PID"

# 等待启动
sleep 1

echo
echo "=== 开始性能测试 ==="
echo

# 测试函数
test_connection() {
    local url=$1
    local name=$2
    echo "测试 $name: $url"
    
    # 使用 time 命令测量时间
    time curl -s --socks5 127.0.0.1:1080 \
        --connect-timeout 10 \
        --max-time 30 \
        "$url" > /dev/null
    
    if [ $? -eq 0 ]; then
        echo "✓ $name 成功"
    else
        echo "✗ $name 失败"
    fi
    echo
}

# httpbin.org 国内访问速度慢，推荐使用下面的本地搭建的 HTTPBin
# docker run -d -p 8080:8080 docker.io/mccutchen/go-httpbin  # 体积小：16.1 MB
# docker run -d -p 80:80 docker.io/kennethreitz/httpbin  # 体积大：204.2 MB，但美观，首页是swagger

# site_address="http://httpbin.org"
site_address="http://172.17.16.174:8080"

# 第一轮测试 - 建立连接
echo "=== 第一轮测试：建立连接 ==="
test_connection "$site_address/get" "HTTPBin GET"
test_connection "$site_address/ip" "HTTPBin IP"
test_connection "$site_address/user-agent" "HTTPBin User-Agent"

# 等待一下让连接进入空闲状态
echo "等待 5 秒让连接进入空闲状态..."
sleep 5

# 第二轮测试 - 测试连接重用
echo "=== 第二轮测试：连接重用 ==="
test_connection "$site_address/get" "HTTPBin GET (重用)"
test_connection "$site_address/ip" "HTTPBin IP (重用)"
test_connection "$site_address/user-agent" "HTTPBin User-Agent (重用)"

# 第三轮测试 - 并发测试
echo "=== 第三轮测试：并发连接 ==="
echo "同时发起 5 个请求..."

for i in {1..5}; do
    (
        test_connection "$site_address/delay/1" "并发测试 $i"
    ) &
done

# 等待所有并发请求完成
sleep 3

echo
echo "=== 测试完成 ==="
echo "查看 server 日志中的连接池统计信息："
echo "  - 总连接数 (Total)"
echo "  - 活跃连接数 (Active)" 
echo "  - 重用连接数 (Reused)"
echo "  - 新建连接数 (New)"
echo "  - 清理连接数 (Cleaned)"
echo

echo "按 Ctrl+C 停止服务"

# 等待用户中断
trap "echo; echo '停止服务...'; kill $SERVER_PID $CLIENT_PID 2>/dev/null; exit 0" INT

wait
