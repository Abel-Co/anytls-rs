use bytes::{Buf, BufMut, Bytes, BytesMut};

// Command constants matching Go implementation
pub const CMD_WASTE: u8 = 0; // Paddings
pub const CMD_SYN: u8 = 1; // stream open
pub const CMD_PSH: u8 = 2; // data push
pub const CMD_FIN: u8 = 3; // stream close, a.k.a EOF mark
pub const CMD_SETTINGS: u8 = 4; // Settings (Client send to Server)
pub const CMD_ALERT: u8 = 5; // Alert
pub const CMD_UPDATE_PADDING_SCHEME: u8 = 6; // update padding scheme
// Since version 2
pub const CMD_SYNACK: u8 = 7; // Server reports to the client that the stream has been opened
pub const CMD_HEART_REQUEST: u8 = 8; // Keep alive command
pub const CMD_HEART_RESPONSE: u8 = 9; // Keep alive command
pub const CMD_SERVER_SETTINGS: u8 = 10; // Settings (Server send to client)

pub const HEADER_OVERHEAD_SIZE: usize = 1 + 4 + 2;

#[derive(Debug, Clone)]
pub struct RawHeader {
    pub cmd: u8,
    pub sid: u32,
    pub length: u16,
}

impl RawHeader {
    pub fn from_bytes(mut data: BytesMut) -> Option<Self> {
        if data.len() < HEADER_OVERHEAD_SIZE {
            return None;
        }

        let cmd = data.get_u8();
        let sid = data.get_u32();
        let length = data.get_u16();

        Some(Self { cmd, sid, length })
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub cmd: u8,
    pub sid: u32,
    pub data: Bytes,
}

impl Frame {
    pub fn new(cmd: u8, sid: u32) -> Self {
        Self {
            cmd,
            sid,
            data: Bytes::new(),
        }
    }

    pub fn with_data(cmd: u8, sid: u32, data: Bytes) -> Self {
        Self { cmd, sid, data }
    }

    pub fn to_bytes(&self) -> Result<Bytes, &'static str> {
        if self.data.len() > u16::MAX as usize {
            return Err("payload too large");
        }
        let mut buf = BytesMut::with_capacity(HEADER_OVERHEAD_SIZE + self.data.len());
        buf.put_u8(self.cmd);
        buf.put_u32(self.sid);
        buf.put_u16(self.data.len() as u16);  // 前面已经检查过长度了。非 silent truncation
        buf.put_slice(&self.data);
        Ok(buf.freeze())
    }

    pub fn from_bytes(mut buf: BytesMut) -> Result<(Self, BytesMut), &'static str> {
        if buf.len() < HEADER_OVERHEAD_SIZE { return Err("need more data for header"); }

        let cmd = buf.get_u8();    // get & advance
        let sid = buf.get_u32();   // get & advance
        let length = buf.get_u16() as usize;   // get & advance

        if buf.len() < length { return Err("need more data for payload"); }

        // let payload = Bytes::copy_from_slice(&data[..length]);
        // data.advance(length);   // only advance the slice

        let payload = buf.split_to(length).freeze(); // split & freeze // zero-copy into Bytes

        Ok((Self {
            cmd,
            sid,
            data: payload,
        }, buf))
    }
}

