#!/bin/bash

cargo build --release --bin anytls-server --bin anytls-client

echo "Starting AnyTLS Server..."
cargo run --bin anytls-server -- -l 0.0.0.0:8443 -p password &
SERVER_PID=$!

echo "Waiting for server to start..."
sleep 1

echo "Starting AnyTLS Client..."
cargo run --bin anytls-client -- -l 127.0.0.1:1080 -s 127.0.0.1:8443 -p password &
CLIENT_PID=$!

echo "Waiting for client to start..."
sleep 1

echo "Testing SOCKS5 proxy with curl..."
# curl -x socks5h://127.0.0.1:1080 -I https://www.jd.com/ --connect-timeout 10
curl --connect-timeout 1 --max-time 2 -w "time_total:  %{time_total}\n" -x socks5h://127.0.0.1:1080 -I https://www.jd.com/

echo "Cleaning up..."
kill $CLIENT_PID 2>/dev/null
kill $SERVER_PID 2>/dev/null
wait
