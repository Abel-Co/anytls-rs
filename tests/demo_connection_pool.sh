#!/bin/bash

# 连接池功能演示脚本

echo "🚀 AnyTLS 连接池功能演示"
echo "================================"
echo

# 检查编译
if [ ! -f "target/debug/anytls-server" ] || [ ! -f "target/debug/anytls-client" ]; then
    echo "📦 编译项目..."
    cargo build --bin anytls-server --bin anytls-client-optimized
    echo
fi

echo "🔧 启动 Server (带连接池)..."
echo "配置参数："
echo "  - 最大连接数: 50"
echo "  - 最大空闲时间: 60秒" 
echo "  - 最小空闲连接: 3"
echo

binaries_path="/Users/Abel/Apps/libs/rust/target/release"

# 启动 server
RUST_LOG=info $binaries_path/anytls-server \
    --listen 0.0.0.0:8443 \
    --password "demo123" \
    --max-outbound-connections 50 \
    --max-idle-time-secs 60 \
    --min-idle-connections 3 &

SERVER_PID=$!
echo "✅ Server 已启动 (PID: $SERVER_PID)"
sleep 2

echo
echo "🔧 启动 Client..."
RUST_LOG=warn $binaries_path/anytls-client-optimized \
    --listen 127.0.0.1:1080 \
    --server 127.0.0.1:8443 \
    --password "demo123" &

CLIENT_PID=$!
echo "✅ Client 已启动 (PID: $CLIENT_PID)"
sleep 2

echo
echo "🧪 开始连接池测试..."
echo

# 测试函数
test_url() {
    local url=$1
    local name=$2
    local round=$3
    
    echo "🔗 [$round] 测试 $name"
    echo "   URL: $url"
    
    start_time=$(date +%s%3N | sed 's/[^0-9]*//g')
    
    response=$(curl -s --socks5 127.0.0.1:1080 \
        --connect-timeout 10 \
        --max-time 15 \
        "$url" 2>/dev/null)
    
    end_time=$(date +%s%3N | sed 's/[^0-9]*//g')
    duration=$((end_time - start_time))
    
    if [ $? -eq 0 ] && [ -n "$response" ]; then
        echo "   ✅ 成功 (${duration} ms)"
        return 0
    else
        echo "   ❌ 失败"
        return 1
    fi
}

# 第一轮：建立连接
echo "📊 第一轮测试：建立新连接"
echo "------------------------"
test_url "http://httpbin.org/get" "HTTPBin GET" "1.1"
test_url "http://httpbin.org/ip" "HTTPBin IP" "1.2" 
test_url "http://httpbin.org/user-agent" "HTTPBin User-Agent" "1.3"

echo
echo "⏳ 等待 3 秒让连接进入空闲状态..."
sleep 3

# 第二轮：测试连接重用
echo
echo "📊 第二轮测试：连接重用"
echo "------------------------"
test_url "http://httpbin.org/get" "HTTPBin GET" "2.1"
test_url "http://httpbin.org/ip" "HTTPBin IP" "2.2"
test_url "http://httpbin.org/user-agent" "HTTPBin User-Agent" "2.3"

echo
echo "⏳ 等待 2 秒..."
sleep 2

# 第三轮：并发测试
echo
echo "📊 第三轮测试：并发连接"
echo "------------------------"
echo "同时发起 3 个请求..."

for i in {1..3}; do
    (
        test_url "http://httpbin.org/delay/1" "并发测试 $i" "3.$i"
    ) &
done

sleep 3

echo
echo "⏳ 等待 2 秒..."
sleep 2

# 第四轮：不同目标测试
echo
echo "📊 第四轮测试：不同目标"
echo "------------------------"
test_url "http://httpbin.org/get" "HTTPBin" "4.1"
test_url "https://www.google.com" "Google" "4.2"
test_url "http://httpbin.org/ip" "HTTPBin" "4.3"

echo
echo "📈 测试完成！"
echo
echo "💡 观察要点："
echo "   - 第一轮：建立新连接，耗时较长"
echo "   - 第二轮：重用连接，耗时较短"
echo "   - 第三轮：并发测试，展示多连接管理"
echo "   - 第四轮：不同目标，展示分组管理"
echo
echo "📊 查看 Server 日志中的连接池统计："
echo "   - Total: 总连接数"
echo "   - Reused: 重用连接数"
echo "   - New: 新建连接数"
echo "   - Active: 活跃连接数"
echo

echo "按 Ctrl+C 停止演示"

# 清理函数
cleanup() {
    echo
    echo "🛑 停止服务..."
    kill $SERVER_PID $CLIENT_PID 2>/dev/null
    echo "✅ 演示结束"
    exit 0
}

trap cleanup INT

# 等待用户中断
sleep 3
