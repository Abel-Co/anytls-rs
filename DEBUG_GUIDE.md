# AnyTLS 调试指南

## 系统架构

```
curl → anytls-client (SOCKS5) → anytls-server (AnyTLS) → 目标服务器
```

## 启动步骤

### 1. 启动服务器
```bash
export PATH=~/.bin:$PATH && server -l 0.0.0.0:8443 -p password
```

### 2. 启动客户端
```bash
export RUST_LOG=debug 
cargo run --bin anytls-client -- -l 0.0.0.0:1080 -s 127.0.0.1:8443 -p password
```

### 3. 运行测试客户端
```bash
cargo run --bin test_client
```

或者使用调试脚本：
```bash
./debug_test.sh
```

### 3. 测试连接
```bash
curl --connect-timeout 2 --max-time 2 -w "time_total:  %{time_total}\n" -x socks5h://127.0.0.1:1080 -I https://www.jd.com/
```

## 调试技巧

### 2. 检查网络连接
```bash
# 检查端口监听
lsof -i -nP | grep -E ':(1080|8443).*LISTEN'
netstat -an | grep -E '.(1080|8443).*LISTEN'
```

3. **使用 tcpdump 抓包**：
```bash
sudo tcpdump -i lo0 -n port 8443 or port 1080
```
   
### 3. 使用 tcpdump 抓包
```bash
# 抓取 AnyTLS 服务器流量
sudo tcpdump -i lo0 -n port 8443

# 抓取 SOCKS5 客户端流量
sudo tcpdump -i lo0 -n port 1080
```

## 停止现有进程
```shell
pkill -f "server.*8443" 2>/dev/null
pkill -f "anytls-client" 2>/dev/null
```

## 预期行为
1. **HTTP 响应**: 收到 `HTTP/1.1 200 OK` 响应

