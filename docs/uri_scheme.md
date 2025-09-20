# URI Scheme Documentation

## Overview

AnyTLS supports URI-based configuration for easy integration with proxy clients and configuration management tools.

## URI Format

```
anytls://[username:password@]hostname[:port][?param1=value1&param2=value2]
```

## Parameters

### Required Parameters

- `hostname`: Server hostname or IP address
- `password`: Authentication password

### Optional Parameters

- `username`: Username (currently unused, reserved for future use)
- `port`: Server port (default: 8443)
- `sni`: TLS SNI (Server Name Indication)
- `padding-scheme`: Custom padding scheme file path
- `log-level`: Logging level (trace, debug, info, warn, error)

## Examples

### Basic Configuration

```
anytls://example.com:8443?password=mypassword
```

### With SNI

```
anytls://example.com:8443?password=mypassword&sni=example.com
```

### With Custom Padding Scheme

```
anytls://example.com:8443?password=mypassword&padding-scheme=/path/to/scheme.txt
```

### With Debug Logging

```
anytls://example.com:8443?password=mypassword&log-level=debug
```

### Full Configuration

```
anytls://user:mypassword@example.com:8443?sni=example.com&padding-scheme=/path/to/scheme.txt&log-level=info
```

## Integration Examples

### Sing-box Configuration

```json
{
  "outbounds": [
    {
      "type": "anytls",
      "tag": "anytls-out",
      "server": "example.com",
      "server_port": 8443,
      "password": "mypassword",
      "sni": "example.com"
    }
  ]
}
```

### Mihomo Configuration

```yaml
proxies:
  - name: "anytls"
    type: anytls
    server: example.com
    port: 8443
    password: mypassword
    sni: example.com
```

### Shadowrocket Configuration

```
anytls://example.com:8443?password=mypassword&sni=example.com
```

## Padding Scheme Format

Custom padding schemes can be specified via the `padding-scheme` parameter. The scheme file should follow this format:

```
stop=8
0=30-30
1=100-400
2=400-500,c,500-1000,c,500-1000,c,500-1000,c,500-1000
3=9-9,500-1000
4=500-1000
5=500-1000
6=500-1000
7=500-1000
```

### Padding Scheme Parameters

- `stop`: Number of packets to apply padding to (0-based)
- `N=range`: Packet size range for packet N
- `c`: Check mark - stop padding if no more data

### Range Format

- `min-max`: Random size between min and max (inclusive)
- `size`: Fixed size
- `c`: Check mark

## Environment Variables

Some parameters can also be set via environment variables:

- `ANYTLS_PASSWORD`: Default password
- `ANYTLS_SNI`: Default SNI
- `ANYTLS_LOG_LEVEL`: Default log level
- `ANYTLS_PADDING_SCHEME`: Default padding scheme file

## Security Considerations

### Password Handling

- Passwords in URIs are URL-encoded
- Consider using environment variables for sensitive data
- Avoid logging URIs with passwords

### SNI Security

- Use SNI to match your server certificate
- Consider using non-standard SNI values
- Monitor for SNI-based blocking

### Padding Scheme Security

- Use different padding schemes for different clients
- Rotate padding schemes regularly
- Monitor for pattern detection

## Troubleshooting

### URI Parsing Errors

- Ensure proper URL encoding
- Check parameter names and values
- Verify required parameters are present

### Connection Issues

- Verify server address and port
- Check password correctness
- Ensure SNI matches server certificate

### Performance Issues

- Adjust padding scheme parameters
- Monitor connection reuse
- Check network latency

## Implementation Notes

### URL Encoding

Special characters in passwords and other parameters should be URL-encoded:

- Space: `%20`
- `@`: `%40`
- `:`: `%3A`
- `?`: `%3F`
- `&`: `%26`
- `=`: `%3D`

### Parameter Validation

- Port numbers must be valid (1-65535)
- Log levels must be valid (trace, debug, info, warn, error)
- Padding scheme files must exist and be readable

### Default Values

- Port: 8443
- Log level: info
- SNI: hostname (if not specified)
- Padding scheme: built-in default
