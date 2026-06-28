use super::close_reason::is_expected_close_error;
use super::core::Session;
use crate::proxy::session::frame::{Frame, RawHeader, CMD_WASTE, HEADER_OVERHEAD_SIZE};
use bytes::{Buf, BufMut, BytesMut};
use std::io;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

impl Session {
    pub(super) async fn run_writer_loop(self: Arc<Self>, writer_rx: &mut mpsc::Receiver<Frame>) {
        loop {
            tokio::select! {
                _ = self.close_notify.notified() => break,
                maybe_frame = writer_rx.recv() => {
                    match maybe_frame {
                        Some(frame) => {
                            if let Err(e) = self.write_frame(frame).await {
                                if is_expected_close_error(&e) {
                                    log::debug!("Session writer loop ended: {}", e);
                                } else {
                                    log::error!("Session writer loop error: {}", e);
                                }
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    }

    async fn write_frame(&self, frame: Frame) -> io::Result<usize> {
        let frame_len = frame_len(&frame);
        let mut conn_guard = self.conn_w.lock().await;
        let conn = conn_guard.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "session write half closed")
        })?;

        if self.send_padding.load(Ordering::Acquire) {
            self.write_with_padding(conn, frame).await?;
        } else {
            write_frame_to(conn, frame).await?;
        }
        Ok(frame_len)
    }

    async fn write_with_padding<W>(&self, conn: &mut W, frame: Frame) -> io::Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        let pkt = self.pkt_counter.fetch_add(1, Ordering::AcqRel);
        let data_len = frame_len(&frame);
        if pkt >= self.padding.stop() {
            self.send_padding.store(false, Ordering::Release);
            write_frame_to(conn, frame).await?;
            return Ok(());
        }

        write_frame_to(conn, frame).await?;
        let pkt_sizes = self.padding.generate_record_payload_sizes(pkt);
        let mut payload_remaining = data_len;
        for size in pkt_sizes {
            if size == crate::proxy::padding::CHECK_MARK {
                if payload_remaining == 0 {
                    break;
                }
                continue;
            }
            let target_payload = size as usize;
            let consumed = payload_remaining.min(target_payload);
            payload_remaining -= consumed;
            if target_payload > consumed + HEADER_OVERHEAD_SIZE {
                let waste_payload_len = target_payload - consumed - HEADER_OVERHEAD_SIZE;
                let mut waste = BytesMut::with_capacity(HEADER_OVERHEAD_SIZE + waste_payload_len);
                waste.put_u8(CMD_WASTE);
                waste.put_u32(0);
                waste.put_u16(waste_payload_len as u16);
                waste.extend_from_slice(&self.padding.rng_vec(waste_payload_len));
                conn.write_all(&waste).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn recv_loop(&self) -> io::Result<()> {
        let mut header_buf = [0u8; HEADER_OVERHEAD_SIZE];
        loop {
            if self.is_closed() {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Session closed"));
            }

            let (cmd, sid, data) = {
                let mut conn_guard = self.conn_r.lock().await;
                let conn = conn_guard.as_mut().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "session read half closed")
                })?;
                conn.read_exact(&mut header_buf).await?;
                let header = RawHeader::from_bytes(&header_buf)?;
                let mut data = BytesMut::with_capacity(header.length as usize);
                if header.length > 0 {
                    data.resize(header.length as usize, 0);
                    conn.read_exact(&mut data).await?;
                }
                (header.cmd, header.sid, data.freeze())
            };
            self.handle_frame(cmd, sid, data).await?;
        }
    }
}

pub(super) async fn write_frame_to<W>(conn: &mut W, frame: Frame) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let header = frame_header(&frame);
    let mut data = Buf::chain(&header[..], frame.data);
    conn.write_all_buf(&mut data).await
}

fn frame_header(frame: &Frame) -> [u8; HEADER_OVERHEAD_SIZE] {
    let mut header = [0u8; HEADER_OVERHEAD_SIZE];
    header[0] = frame.cmd;
    header[1..5].copy_from_slice(&frame.sid.to_be_bytes());
    header[5..7].copy_from_slice(&(frame.data.len() as u16).to_be_bytes());
    header
}

fn frame_len(frame: &Frame) -> usize {
    HEADER_OVERHEAD_SIZE + frame.data.len()
}
