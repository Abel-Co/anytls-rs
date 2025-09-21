#!/bin/bash

# 无锁连接池性能测试脚本

echo "⚡ AnyTLS 无锁连接池性能测试"
echo "================================"
echo

# 编译项目
echo "📦 编译项目..."
cargo build --release --bin anytls-server --bin anytls-client-optimized

binaries_path="./target/release"

echo
echo "🧪 开始性能对比测试..."
echo

# 测试函数
run_test() {
    local pool_type=$1
    local pool_name=$2
    local test_duration=$3
    
    echo "🔧 测试 $pool_name (类型: $pool_type)"
    echo "----------------------------------------"
    
    # 启动 server
    RUST_LOG=warn $binaries_path/anytls-server \
        --listen 0.0.0.0:8443 \
        --password "test123" \
        --max-outbound-connections 50 \
        --max-idle-time-secs 60 \
        --min-idle-connections 3 \
        --pool-type "$pool_type" &
    
    SERVER_PID=$!
    echo "✅ Server 已启动 (PID: $SERVER_PID, 类型: $pool_type)"
    
    # 等待 server 启动
    sleep 1
    
    # 启动 client
    RUST_LOG=warn $binaries_path/anytls-client-optimized \
        --listen 127.0.0.1:1080 \
        --server 127.0.0.1:8443 \
        --password "test123" &
    
    CLIENT_PID=$!
    echo "✅ Client 已启动 (PID: $CLIENT_PID)"
    
    # 等待 client 启动
    sleep 1
    
    echo "🚀 开始性能测试..."
    
    # 测试函数
    test_url() {
        local url=$1
        local name=$2
        local round=$3
        
        echo "  🔗 [$round] $name"
        
        start_time=$(date +%s%3N | sed 's/[^0-9]*//g')
        
        response=$(curl -s --socks5 127.0.0.1:1080 \
            --connect-timeout 5 \
            --max-time 10 \
            "$url" 2>/dev/null)
        
        end_time=$(date +%s%3N | sed 's/[^0-9]*//g')
        duration=$((end_time - start_time))
        
        if [ $? -eq 0 ] && [ -n "$response" ]; then
            echo "    ✅ 成功 (Duration: ${duration} ms)"
            return 0
        else
            echo "    ❌ 失败"
            return 1
        fi
    }
    
    # site_address="http://172.17.16.174:8080"
    site_address="http://httpbin.org"

    # 第一轮：建立连接
    echo "  📊 第一轮：建立新连接"
    test_url "$site_address/get" "HTTPBin GET" "1.1"
    test_url "$site_address/ip" "HTTPBin IP" "1.2"
    test_url "$site_address/user-agent" "HTTPBin User-Agent" "1.3"
    
    # 等待连接进入空闲状态
    sleep 2
    
    # 第二轮：测试连接重用
    echo "  📊 第二轮：连接重用"
    test_url "$site_address/get" "HTTPBin GET" "2.1"
    test_url "$site_address/ip" "HTTPBin IP" "2.2"
    test_url "$site_address/user-agent" "HTTPBin User-Agent" "2.3"
    
    # 第三轮：并发测试
    echo "  📊 第三轮：并发连接"
    for i in {1..5}; do
        (
            test_url "$site_address/delay/1" "并发测试 $i" "3.$i"
        ) &
    done
    sleep 2
    
    # 第四轮：压力测试
    echo "  📊 第四轮：压力测试"
    for i in {1..10}; do
        (
            test_url "$site_address/get" "压力测试 $i" "4.$i"
        ) &
    done
    sleep 2
    
    echo "  ✅ $pool_name 测试完成"
    echo
    
    # 停止服务
    echo "🛑 停止服务..."
    kill $SERVER_PID $CLIENT_PID 2>/dev/null
    sleep 1
    
    echo "----------------------------------------"
    echo
}

# 运行测试
echo "🔒 测试原始锁版本"
run_test "lock" "原始锁版本" 30

echo "⚡ 测试无锁版本"
run_test "lockfree" "无锁版本" 30

echo "🚀 测试高性能版本"
run_test "highperf" "高性能版本" 30

echo "📊 测试完成！"
echo
echo "💡 观察要点："
echo "  - 连接建立时间：第一轮测试"
echo "  - 连接重用效果：第二轮测试"
echo "  - 并发处理能力：第三轮测试"
echo "  - 压力测试表现：第四轮测试"
echo
echo "📈 性能提升预期："
echo "  - 无锁版本：减少锁竞争，提升并发性能"
echo "  - 高性能版本：进一步优化，减少内存分配"
echo
echo "🔍 查看 Server 日志中的连接池统计信息来验证效果"
