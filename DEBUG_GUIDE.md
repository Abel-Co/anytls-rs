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

成功的测试应该显示：

1. **HTTP 响应**: 收到 `HTTP/1.1 200 OK` 响应


## 调试日志说明

### 测试客户端日志 (`[Test]` 前缀)
- 连接到 SOCKS5 代理
- 执行 SOCKS5 握手
- 发送 HTTP 请求到目标服务器
- 接收和显示响应

### AnyTLS 客户端日志 (`[Client]` 前缀)
- SOCKS5 握手处理
- 目标地址解析
- 到 AnyTLS 服务器的连接
- TLS 握手过程
- AnyTLS 协议认证

### AnyTLS 会话日志 (`[Session]` 前缀)
- 客户端设置发送
- 帧接收和处理
- 流管理

## 常见问题排查

### 1. 连接被拒绝
- 检查服务器是否正在运行
- 检查端口是否正确 (8443 for server, 1080 for client)
- 检查防火墙设置

### 2. SOCKS5 握手失败
- 检查客户端是否正在监听 1080 端口
- 检查 SOCKS5 协议实现

### 3. TLS 握手失败
- 检查服务器证书配置
- 检查 SNI 设置
- 查看 TLS 相关错误日志

### 4. AnyTLS 认证失败
- 检查密码是否正确
- 检查认证协议实现
- 查看认证相关日志

### 5. 数据转发问题
- 检查 Stream 创建是否成功
- 检查数据帧处理
- 查看数据转发日志

## 调试技巧

1. **使用 debug 日志级别**：
   ```bash
   RUST_LOG=debug cargo run --bin test_client
   ```

2. **检查网络连接**：
   ```bash
   netstat -an | grep -E "(8443|1080)"
   ```

3. **使用 tcpdump 抓包**：
   ```bash
   sudo tcpdump -i lo0 -n port 8443 or port 1080
   ```

4. **逐步测试**：
   - 先测试 SOCKS5 连接
   - 再测试 TLS 连接
   - 最后测试 AnyTLS 协议

## 预期行为

成功的测试应该显示：
1. SOCKS5 握手成功
2. TLS 连接建立
3. AnyTLS 认证成功
4. HTTP 请求发送
5. HTTP 响应接收

如果所有步骤都成功，说明 AnyTLS 代理工作正常。
