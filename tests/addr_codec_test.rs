use anytls_rs::proxy::addr_codec::{build_socks_addr, AddressType, SocksAddr};

#[test]
fn build_socks_addr_domain() {
    let addr = SocksAddr {
        atyp: AddressType::Domain,
        host: "www.google.com".to_string(),
        port: 443,
    };
    let out = build_socks_addr(&addr).unwrap();

    assert_eq!(out[0], 0x03);
    assert_eq!(out[1] as usize, "www.google.com".len());
    assert_eq!(&out[2..2 + "www.google.com".len()], b"www.google.com");
    assert_eq!(&out[out.len() - 2..], &443u16.to_be_bytes());
}

#[test]
fn build_socks_addr_ipv4() {
    let addr = SocksAddr {
        atyp: AddressType::Ipv4,
        host: "1.2.3.4".to_string(),
        port: 8080,
    };
    let out = build_socks_addr(&addr).unwrap();

    assert_eq!(out, vec![0x01, 1, 2, 3, 4, 0x1f, 0x90]);
}
