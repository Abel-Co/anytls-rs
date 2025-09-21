# 远程 Linux 开发环境指南

## 方案对比

| 方案 | 优势 | 劣势 | 适用场景 | 推荐指数 |
|------|------|------|----------|----------|
| **Cursor Remote-SSH** | 完整 IDE 体验、调试支持、扩展支持 | 需要稳定网络 | 个人开发、小团队 | ⭐⭐⭐⭐⭐ |
| **Code-Server** | Web 访问、无需本地安装 | 性能略低、功能受限 | 临时访问、演示 | ⭐⭐⭐ |
| **GitHub Codespaces** | 云端环境、即开即用 | 有使用限制、成本 | 开源项目、临时开发 | ⭐⭐⭐⭐ |
| **tmux + SSH** | 轻量级、终端友好 | 无 GUI 支持 | 服务器管理、CI/CD | ⭐⭐⭐ |

## 推荐方案：Cursor Remote-SSH

### 1. 环境准备

#### 1.1 远程服务器要求
- **操作系统**: Ubuntu 20.04+ / CentOS 8+ / Debian 11+
- **内存**: 至少 2GB RAM
- **存储**: 至少 10GB 可用空间
- **网络**: 稳定的互联网连接

#### 1.2 本地环境要求
- **macOS**: 10.15+ (支持 Cursor)
- **网络**: 稳定的互联网连接
- **SSH**: 已配置密钥认证

### 2. 快速开始

#### 2.1 配置远程服务器
```bash
# 1. 上传配置脚本到服务器
scp setup-remote-server.sh user@server-ip:~/

# 2. 在服务器上运行配置脚本
ssh user@server-ip
chmod +x setup-remote-server.sh
./setup-remote-server.sh
```

#### 2.2 配置本地 SSH
```bash
# 编辑 SSH 配置
nano ~/.ssh/config

# 添加以下内容：
Host anytls-dev
    HostName your-server-ip
    User your-username
    Port 22
    IdentityFile ~/.ssh/id_rsa
    ServerAliveInterval 60
    ServerAliveCountMax 3
    ForwardAgent yes
    LocalForward 8443 localhost:8443
```

#### 2.3 连接 Cursor
1. 打开 Cursor
2. 按 `Cmd+Shift+P`
3. 选择 "Remote-SSH: Connect to Host"
4. 选择 "anytls-dev"
5. 打开项目目录: `~/projects/anytls-rs`

### 3. 开发工作流

#### 3.1 日常开发
```bash
# 在远程服务器上
cd ~/projects/anytls-rs

# 构建项目
cargo build

# 运行服务器
cargo run --bin anytls-server -- --listen 0.0.0.0:8443 --password "test123" --pool-type lockfree

# 运行客户端测试
cargo run --bin anytls-client -- --server 127.0.0.1:8443 --password "test123"
```

#### 3.2 调试和监控
```bash
# 查看服务器日志
journalctl -u anytls-server -f

# 监控系统资源
htop

# 网络监控
tcpdump -i any port 8443

# 性能分析
perf top -p $(pgrep anytls-server)
```

### 4. 高级配置

#### 4.1 端口转发
```bash
# 在本地 macOS 上
ssh -L 8443:localhost:8443 anytls-dev

# 现在可以通过 http://localhost:8443 访问远程服务
```

#### 4.2 文件同步
```bash
# 使用 rsync 同步代码
rsync -avz --exclude='target/' ./ anytls-dev:~/projects/anytls-rs/

# 使用 git 同步
git add .
git commit -m "Update code"
git push origin main
ssh anytls-dev "cd ~/projects/anytls-rs && git pull"
```

#### 4.3 多环境管理
```bash
# 开发环境
ssh anytls-dev

# 测试环境
ssh anytls-test

# 生产环境
ssh anytls-prod
```

### 5. 故障排除

#### 5.1 连接问题
```bash
# 测试 SSH 连接
ssh -v anytls-dev

# 检查端口转发
netstat -an | grep 8443

# 重启 SSH 服务
sudo systemctl restart ssh
```

#### 5.2 性能问题
```bash
# 检查系统资源
htop
df -h
free -h

# 检查网络延迟
ping your-server-ip
traceroute your-server-ip
```

#### 5.3 项目问题
```bash
# 清理构建缓存
cargo clean

# 更新依赖
cargo update

# 检查代码
cargo check
cargo clippy
cargo test
```

## 最佳实践

### 1. 安全配置
- 使用 SSH 密钥认证
- 配置防火墙规则
- 定期更新系统包
- 使用非标准端口

### 2. 性能优化
- 配置 SSH 连接复用
- 使用压缩传输
- 优化网络延迟
- 合理分配资源

### 3. 开发效率
- 使用 tmux 保持会话
- 配置自动补全
- 设置别名和函数
- 使用版本控制

### 4. 团队协作
- 统一开发环境
- 共享配置脚本
- 文档化流程
- 代码审查机制
