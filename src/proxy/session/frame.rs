use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::io;

// CMD 指令定义
pub const CMD_WASTE: u8 = 0;               // Paddings
pub const CMD_SYN: u8 = 1;                 // stream open
pub const CMD_PSH: u8 = 2;                 // data push
pub const CMD_FIN: u8 = 3;                 // stream close, a.k.a EOF mark
pub const CMD_SETTINGS: u8 = 4;            // Settings (Client send to Server)
pub const CMD_ALERT: u8 = 5;               // Alert
pub const CMD_UPDATE_PADDING_SCHEME: u8 = 6; // update padding scheme
// Since version 2
pub const CMD_SYNACK: u8 = 7;              // Server reports to the client that the stream has been opened
pub const CMD_HEART_REQUEST: u8 = 8;       // Keep alive command
pub const CMD_HEART_RESPONSE: u8 = 9;      // Keep alive command
pub const CMD_SERVER_SETTINGS: u8 = 10;    // Settings (Server send to client)

pub const HEADER_OVERHEAD_SIZE: usize = 1 + 4 + 2; // cmd(1) + sid(4) + length(2)

/// 原始头部结构
#[derive(Debug, Clone, Copy)]
pub struct RawHeader {
    pub cmd: u8,
    pub sid: u32,
    pub length: u16,
}

impl RawHeader {
    pub fn from_bytes(buf: &[u8]) -> io::Result<Self> {
        if buf.len() < HEADER_OVERHEAD_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Insufficient data for header",
            ));
        }

        let mut cursor = std::io::Cursor::new(buf);
        let cmd = cursor.get_u8();
        let sid = cursor.get_u32();
        let length = cursor.get_u16();

        Ok(Self { cmd, sid, length })
    }
}

/// Frame 结构体，定义从或要复用到单个连接中的数据包
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

    /// 从字节流中解析 Frame
    pub fn from_bytes(mut buf: &[u8]) -> io::Result<Self> {
        if buf.len() < HEADER_OVERHEAD_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Insufficient data for frame header",
            ));
        }

        let cmd = buf.get_u8();
        let sid = buf.get_u32();
        let length = buf.get_u16() as usize;

        if buf.len() < length {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Insufficient data for frame payload",
            ));
        }

        let data = Bytes::copy_from_slice(&buf[..length]);

        Ok(Self { cmd, sid, data })
    }

    /// 将 Frame 序列化为字节流
    pub fn to_bytes(&self) -> BytesMut {
        let mut buf = BytesMut::with_capacity(HEADER_OVERHEAD_SIZE + self.data.len());
        buf.put_u8(self.cmd);
        buf.put_u32(self.sid);
        buf.put_u16(self.data.len() as u16);
        buf.put_slice(&self.data);
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_serialization() {
        let frame = Frame::with_data(CMD_PSH, 123, Bytes::from("hello"));
        let bytes = frame.to_bytes();
        let parsed = Frame::from_bytes(&bytes).unwrap();
        
        assert_eq!(frame.cmd, parsed.cmd);
        assert_eq!(frame.sid, parsed.sid);
        assert_eq!(frame.data, parsed.data);
    }
}
