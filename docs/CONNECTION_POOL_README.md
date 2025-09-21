# AnyTLS 连接池功能

## 概述

现在 AnyTLS Server 支持外网连接池功能，可以显著提升代理性能，特别是在频繁访问相同目标网站时。

## 功能特性

### 1. 外网连接池
- **连接复用**：相同目标地址的连接会被复用，避免重复建立连接
- **连接管理**：自动管理连接的生命周期，包括创建、重用、清理
- **按目标分组**：不同目标地址的连接分别管理，互不影响

### 2. 连接池配置
- `--max-outbound-connections`：每个目标的最大连接数（默认：100）
- `--max-idle-time-secs`：连接的最大空闲时间，超时后会被清理（默认：300秒）
- `--min-idle-connections`：每个目标最少保持的空闲连接数（默认：5）

### 3. 统计信息
Server 会每分钟打印连接池统计信息：
- **Total**：总连接数
- **Active**：当前活跃连接数
- **Reused**：重用连接数
- **New**：新建连接数
- **Cleaned**：清理连接数

## 使用方法

### 1. 启动 Server（带连接池）

```bash
# 使用默认配置
cargo run --bin anytls-server -- -l 0.0.0.0:8443 -p password

# 自定义连接池配置
cargo run --bin anytls-server -- -l 0.0.0.0:8443 -p password \
    --max-outbound-connections 200 \
    --max-idle-time-secs 600 \
    --min-idle-connections 10
```

### 2. 启动 Client

```bash
cargo run --bin anytls-client -- -l 127.0.0.1:1080 -s 127.0.0.1:8443 -p password
```

### 3. 测试连接池效果

使用提供的测试脚本：

```bash
# 基础测试
./test_connection_pool.sh

# 性能测试
./test_pool_performance.sh
```

## 性能优势

### 1. 连接重用
- 避免重复的 TCP 握手过程
- 减少连接建立延迟
- 降低服务器资源消耗

### 2. 连接管理
- 自动清理空闲连接，避免资源浪费
- 保持最小连接数，确保快速响应
- 按目标分组管理，提高效率

### 3. 统计监控
- 实时监控连接池状态
- 便于性能调优和问题排查
- 提供详细的连接使用统计

## 配置建议

### 1. 高并发场景 (吞吐量优先)
```bash
--pool-type highperf \
--max-outbound-connections 500 \
--max-idle-time-secs 300 \
--min-idle-connections 20
```
**适用**：> 500 连接/秒，追求最大吞吐量
**原理**：大连接池支持高并发，短空闲时间快速回收资源

### 2. 低延迟场景 (响应时间优先)
```bash
--pool-type lockfree \
--max-outbound-connections 100 \
--max-idle-time-secs 600 \
--min-idle-connections 10
```
**适用**：< 200 连接/秒，追求最低延迟
**原理**：长空闲时间最大化连接重用，减少连接建立延迟

### 3. 资源受限场景 (内存优先)
```bash
--pool-type lock \
--max-outbound-connections 50 \
--max-idle-time-secs 120 \
--min-idle-connections 3
```
**适用**：< 100 连接/秒，内存受限环境
**原理**：小连接池减少内存占用，简单可靠

### 4. 平衡场景 (推荐)
```bash
--pool-type lockfree \
--max-outbound-connections 200 \
--max-idle-time-secs 300 \
--min-idle-connections 5
```
**适用**：100-500 连接/秒，平衡性能和资源消耗
**原理**：适中的参数设置，适合大多数应用场景

## 连接池类型选择指南

| 连接池类型 | 适用场景 | 并发量 | 内存使用 | 延迟 | 复杂度 | 推荐指数 |
|------------|----------|--------|----------|------|--------|----------|
| **lock** | 简单应用、资源受限 | < 100 连接/秒 | 低 | 中等 | 低 | ⭐⭐⭐ |
| **lockfree** | 平衡场景、中等并发 | 100-500 连接/秒 | 中等 | 低 | 中等 | ⭐⭐⭐⭐⭐ |
| **highperf** | 高并发、性能敏感 | > 500 连接/秒 | 高 | 最低 | 高 | ⭐⭐⭐⭐ |

### 选择建议

1. **开发测试阶段**：使用 `lock` 版本，简单可靠
2. **生产环境**：根据实际并发量选择：
   - < 100 连接/秒：`lock`
   - 100-500 连接/秒：`lockfree` (推荐)
   - > 500 连接/秒：`highperf`
3. **不确定时**：选择 `lockfree`，平衡性能和复杂度

## 监控指标

### 1. 连接重用率
```
重用率 = 重用连接数 / (重用连接数 + 新建连接数)
```
- 重用率越高，性能提升越明显
- 建议目标：> 50%

### 2. 连接池利用率
```
利用率 = 活跃连接数 / 总连接数
```
- 利用率适中表示连接池配置合理
- 建议范围：20% - 80%

### 3. 清理频率
- 定期清理空闲连接，避免资源浪费
- 清理频率过高可能表示配置不当

## 注意事项

1. **内存使用**：连接池会占用一定内存，需要根据实际情况调整配置
2. **连接超时**：空闲连接超时后会被清理，需要重新建立
3. **目标限制**：不同目标地址的连接分别管理，不会相互影响
4. **错误处理**：连接出错时会自动清理，不会影响其他连接

## 故障排查

### 1. 连接数过多
- 检查 `max-outbound-connections` 配置
- 观察连接清理情况
- 调整 `max-idle-time-secs` 参数

### 2. 连接重用率低
- 检查目标网站是否支持连接复用
- 调整 `min-idle-connections` 参数
- 观察连接使用模式

### 3. 性能问题
- 查看统计信息中的连接分布
- 调整连接池配置参数
- 检查网络延迟和带宽

## 示例日志

```
[INFO] [Server] Outbound connection pool initialized:
  - Max connections per target: 100
  - Max idle time: 300 seconds
  - Min idle connections: 5

[INFO] [Pool Stats] Total: 15, Active: 3, Reused: 8, New: 7, Cleaned: 2
[INFO] [Pool Status] {"httpbin.org:80": 5, "google.com:443": 3, "example.com:80": 2}
```

这个日志显示：
- 总共创建了 15 个连接
- 当前有 3 个活跃连接
- 重用了 8 次连接
- 新建了 7 个连接
- 清理了 2 个连接
- 连接池中有 3 个不同目标的连接
