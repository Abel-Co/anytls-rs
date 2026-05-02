use super::core::Session;
use crate::proxy::session::frame::{
    Frame, CMD_ALERT, CMD_FIN, CMD_HEART_REQUEST, CMD_HEART_RESPONSE, CMD_PSH, CMD_SERVER_SETTINGS,
    CMD_SETTINGS, CMD_SYN, CMD_SYNACK, CMD_UPDATE_PADDING_SCHEME, CMD_WASTE,
};
use crate::proxy::session::stream::Stream;
use crate::util::string_map::{StringMap, StringMapExt};
use bytes::Bytes;
use std::io;
use std::sync::atomic::Ordering;
use tokio::sync::{mpsc, oneshot};

impl Session {
    pub(super) async fn handle_frame(&self, cmd: u8, sid: u32, data: Bytes) -> io::Result<()> {
        self.touch_activity();
        match cmd {
            CMD_WASTE => Ok(()),
            CMD_PSH => self.handle_psh(sid, data).await,
            CMD_SYN => self.handle_syn(sid).await,
            CMD_SYNACK => self.handle_synack(sid, data).await,
            CMD_FIN => self.handle_fin(sid).await,
            CMD_SETTINGS => self.handle_settings(data).await,
            CMD_ALERT => self.handle_alert(data),
            CMD_UPDATE_PADDING_SCHEME => self.handle_padding_scheme_update_cmd(data).await,
            CMD_HEART_REQUEST => self.handle_heartbeat_request(sid).await,
            CMD_HEART_RESPONSE => Ok(()),
            CMD_SERVER_SETTINGS => self.handle_server_settings_cmd(data).await,
            _ => Ok(()),
        }
    }

    async fn handle_psh(&self, sid: u32, data: Bytes) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let streams = self.streams.read().await;
        if let Some(stream_tx) = streams.get(&sid) {
            if stream_tx.try_send(data).is_err() {
                log::warn!("[Session] Failed to send data to stream {}: channel full", sid);
            }
        }
        Ok(())
    }

    async fn handle_syn(&self, sid: u32) -> io::Result<()> {
        if self.is_client {
            log::warn!("Client received unexpected SYN for stream: {}", sid);
            return Ok(());
        }

        if self.streams.read().await.contains_key(&sid) {
            let err = format!("Stream {} already exists", sid);
            let _ = self
                .write_control_frame(Frame::with_data(CMD_SYNACK, sid, Bytes::from(err)))
                .await;
            return Ok(());
        }

        let (data_tx, data_rx) = mpsc::channel(100);
        let (close_tx, _close_rx) = oneshot::channel();
        let stream = Stream::new(sid, data_rx, self.frame_tx.clone(), close_tx);
        {
            let mut streams = self.streams.write().await;
            streams.insert(sid, data_tx);
        }
        self.stream_count.fetch_add(1, Ordering::AcqRel);

        if let Err(e) = self.write_control_frame(Frame::new(CMD_SYNACK, sid)).await {
            log::error!("Failed to send SYNACK for stream {}: {}", sid, e);
            let mut streams = self.streams.write().await;
            streams.remove(&sid);
            return Ok(());
        }
        log::info!("Stream {} opened successfully", sid);
        if let Some(cb) = &self.on_new_stream {
            cb(stream);
        }
        Ok(())
    }

    async fn handle_synack(&self, sid: u32, data: Bytes) -> io::Result<()> {
        if !self.is_client {
            log::warn!("Server received unexpected SYNACK for stream: {}", sid);
            return Ok(());
        }
        if self.streams.read().await.get(&sid).is_some() && !data.is_empty() {
            let error_msg = String::from_utf8_lossy(&data);
            log::error!("Stream {} open failed: {}", sid, error_msg);
        }
        Ok(())
    }

    async fn handle_fin(&self, sid: u32) -> io::Result<()> {
        let mut streams = self.streams.write().await;
        if streams.remove(&sid).is_some() {
            self.stream_count.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(())
    }

    async fn handle_settings(&self, data: Bytes) -> io::Result<()> {
        if !self.is_client && !data.is_empty() {
            self.handle_client_settings(data).await?;
        }
        Ok(())
    }

    fn handle_alert(&self, data: Bytes) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let alert_msg = String::from_utf8_lossy(&data);
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("Alert received: {}", alert_msg),
        ))
    }

    async fn handle_heartbeat_request(&self, sid: u32) -> io::Result<()> {
        let frame = Frame::new(CMD_HEART_RESPONSE, sid);
        self.write_control_frame(frame).await.map(|_| ())
    }

    async fn handle_server_settings_cmd(&self, data: Bytes) -> io::Result<()> {
        if self.is_client && !data.is_empty() {
            let settings = StringMap::from_bytes(&data);
            if let Some(version) = settings.get("v") {
                if let Ok(v) = version.parse::<u32>() {
                    self.peer_version.store(v, Ordering::Release);
                }
            }
        }
        Ok(())
    }

    async fn handle_client_settings(&self, data: Bytes) -> io::Result<()> {
        let settings = StringMap::from_bytes(&data);
        if let Some(padding_md5) = settings.get("padding-md5") {
            if padding_md5 != self.padding.md5() {
                let raw_scheme = self.padding.raw_scheme.clone();
                let frame = Frame::with_data(CMD_UPDATE_PADDING_SCHEME, 0, Bytes::from(raw_scheme));
                self.write_control_frame(frame).await?;
            }
        }
        if let Some(version) = settings.get("v") {
            if let Ok(v) = version.parse::<u32>() {
                self.peer_version.store(v, Ordering::Release);
                if v >= 2 {
                    let server_settings = StringMap::from([("v".to_string(), "2".to_string())]);
                    let frame = Frame::with_data(
                        CMD_SERVER_SETTINGS,
                        0,
                        Bytes::from(server_settings.to_bytes()),
                    );
                    self.write_control_frame(frame).await?;
                }
            }
        }
        Ok(())
    }

    async fn handle_padding_scheme_update_cmd(&self, data: Bytes) -> io::Result<()> {
        if !self.is_client || data.is_empty() {
            return Ok(());
        }
        if crate::proxy::padding::PaddingFactory::new(&data).is_none() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid padding scheme"));
        }
        Ok(())
    }
}
