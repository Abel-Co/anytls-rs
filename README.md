# AnyTLS-RS

A Rust implementation of the AnyTLS proxy protocol that attempts to mitigate the TLS in TLS fingerprinting problem.

- Flexible packet splitting and padding strategies
- Connection reuse to reduce proxy latency
- Simple configuration

[User FAQ](./docs/faq.md)

[Protocol Documentation](./docs/protocol.md)

[URI Format](./docs/uri_scheme.md)

## Quick Start

### Server

```shell
cargo run --bin anytls-server -- -l 0.0.0.0:8443 -p password
# cargo run --bin anytls-server --release -- -l 0.0.0.0:8443 -p password
```

`0.0.0.0:8443` is the server listening address and port.

### Client

```shell
cargo run --bin anytls-client -- -l 0.0.0.0:1080 -s server_ip:port -p password
# cargo run --bin anytls-client --release -- -l 0.0.0.0:1080 -s 127.0.0.1:8443 -p password
```

`127.0.0.1:1080` is the local SOCKS5 proxy listening address, theoretically supports TCP and UDP (via UDP over TCP transmission).


### 版本/分支

- main
  - anytls-go 的直接转译；
  - time_total:  0.029762
  - Success: client(rs) <=> server(rs)
  - Success: client(rs) <=> server(go)
  - Failure: client(go) <=> server(rs)

- highperf
  - 连接复用、池化
  - time_total:  0.025750

- Glommio
  - glommio运行时，减少跨线程唤醒和全局队列竞争；
  - time_total:  0.024587


### sing-box

https://github.com/SagerNet/sing-box

Merged into the dev-next branch. It contains the anytls protocol server and client.

### mihomo

https://github.com/MetaCubeX/mihomo

Merged into the Alpha branch. It contains the anytls protocol server and client.

### Shadowrocket

Shadowrocket 2.2.65+ implements the anytls protocol client.
