use anytls_rs::proxy::session::{Frame, CMD_PSH};
use bytes::Bytes;

#[test]
fn frame_serialization_round_trip() {
    let frame = Frame::with_data(CMD_PSH, 123, Bytes::from("hello"));
    let bytes = frame.to_bytes();
    let parsed = Frame::from_bytes(&bytes).unwrap();

    assert_eq!(frame.cmd, parsed.cmd);
    assert_eq!(frame.sid, parsed.sid);
    assert_eq!(frame.data, parsed.data);
}
