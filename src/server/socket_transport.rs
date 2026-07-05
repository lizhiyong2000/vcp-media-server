use anyhow::Result;
use bytes::BytesMut;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::time::timeout;
use tracing::{debug, error, info};

pub struct SocketReader<R> {
    reader: R,
    buffer: BytesMut,
    timeout: Option<Duration>,
}

impl<R: AsyncReadExt + Unpin> SocketReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: BytesMut::with_capacity(8192),
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if let Some(t) = self.timeout {
            Ok(timeout(t, self.reader.read(buf)).await??)
        } else {
            Ok(self.reader.read(buf).await?)
        }
    }

    pub async fn read_buf(&mut self) -> Result<usize> {
        self.buffer.clear();
        let mut read_buf = [0u8; 8192];
        let len = self.read(&mut read_buf).await?;
        self.buffer.extend_from_slice(&read_buf[..len]);
        Ok(len)
    }

    pub async fn read_into(&mut self, buf: &mut BytesMut) -> Result<usize> {
        let mut read_buf = [0u8; 8192];
        let len = self.read(&mut read_buf).await?;
        buf.extend_from_slice(&read_buf[..len]);
        Ok(len)
    }

    pub async fn read_until(&mut self, delimiter: &[u8]) -> Result<Vec<u8>> {
        let mut result = Vec::new();
        let mut temp_buf = [0u8; 1024];

        loop {
            let len = self.read(&mut temp_buf).await?;
            if len == 0 {
                break;
            }

            result.extend_from_slice(&temp_buf[..len]);

            if let Some(pos) = result.windows(delimiter.len()).position(|w| w == delimiter) {
                let mut final_result = result[..pos + delimiter.len()].to_vec();
                result.drain(..pos + delimiter.len());
                self.buffer.extend_from_slice(&result);
                return Ok(final_result);
            }
        }

        Ok(result)
    }

    pub async fn read_to_string(&mut self) -> Result<String> {
        let mut result = String::new();
        let mut temp_buf = [0u8; 8192];

        loop {
            let len = self.read(&mut temp_buf).await?;
            if len == 0 {
                break;
            }
            result.push_str(&String::from_utf8_lossy(&temp_buf[..len]));
        }

        Ok(result)
    }

    pub fn buffer(&self) -> &BytesMut {
        &self.buffer
    }

    pub fn buffer_mut(&mut self) -> &mut BytesMut {
        &mut self.buffer
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
}

pub struct SocketWriter<W> {
    writer: W,
    timeout: Option<Duration>,
}

impl<W: AsyncWriteExt + Unpin> SocketWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub async fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if let Some(t) = self.timeout {
            Ok(timeout(t, self.writer.write(buf)).await??)
        } else {
            Ok(self.writer.write(buf).await?)
        }
    }

    pub async fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        if let Some(t) = self.timeout {
            Ok(timeout(t, self.writer.write_all(buf)).await??)
        } else {
            Ok(self.writer.write_all(buf).await?)
        }
    }

    pub async fn flush(&mut self) -> Result<()> {
        if let Some(t) = self.timeout {
            Ok(timeout(t, self.writer.flush()).await??)
        } else {
            Ok(self.writer.flush().await?)
        }
    }

    pub async fn write_and_flush(&mut self, buf: &[u8]) -> Result<()> {
        self.write_all(buf).await?;
        self.flush().await?;
        Ok(())
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

pub struct SocketTransport<R, W> {
    reader: SocketReader<R>,
    writer: SocketWriter<W>,
}

impl<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin> SocketTransport<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: SocketReader::new(reader),
            writer: SocketWriter::new(writer),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.reader = self.reader.with_timeout(timeout);
        self.writer = self.writer.with_timeout(timeout);
        self
    }

    pub fn reader(&mut self) -> &mut SocketReader<R> {
        &mut self.reader
    }

    pub fn writer(&mut self) -> &mut SocketWriter<W> {
        &mut self.writer
    }

    pub fn into_inner(self) -> (R, W) {
        (self.reader.into_inner(), self.writer.into_inner())
    }
}

pub struct BufferedSocketReader<R> {
    reader: SocketReader<R>,
    delimiter: Vec<u8>,
}

impl<R: AsyncReadExt + Unpin> BufferedSocketReader<R> {
    pub fn new(reader: R, delimiter: &[u8]) -> Self {
        Self {
            reader: SocketReader::new(reader),
            delimiter: delimiter.to_vec(),
        }
    }

    pub async fn read_message(&mut self) -> Result<Option<Vec<u8>>> {
        let data = self.reader.read_until(&self.delimiter).await?;
        if data.is_empty() {
            Ok(None)
        } else {
            Ok(Some(data))
        }
    }

    pub async fn read_message_string(&mut self) -> Result<Option<String>> {
        match self.read_message().await? {
            Some(data) => Ok(Some(String::from_utf8_lossy(&data).to_string())),
            None => Ok(None),
        }
    }

    pub fn into_inner(self) -> R {
        self.reader.into_inner()
    }
}

#[derive(Clone)]
pub struct ChannelSocketWriter {
    tx: Sender<Vec<u8>>,
}

impl ChannelSocketWriter {
    pub fn new(tx: Sender<Vec<u8>>) -> Self {
        Self { tx }
    }

    pub async fn write(&self, buf: &[u8]) -> Result<()> {
        self.tx.send(buf.to_vec()).await.map_err(|e| anyhow::anyhow!("Send error: {}", e))
    }

    pub async fn write_all(&self, buf: &[u8]) -> Result<()> {
        self.write(buf).await
    }

    pub fn into_inner(self) -> Sender<Vec<u8>> {
        self.tx
    }

    pub fn sender(&self) -> &Sender<Vec<u8>> {
        &self.tx
    }
}

pub async fn spawn_write_loop<W: AsyncWriteExt + Unpin + Send + 'static>(
    mut writer: W,
    mut rx: Receiver<Vec<u8>>,
) {
    while let Some(data) = rx.recv().await {
        if let Err(e) = writer.write_all(&data).await {
            error!("Write loop error: {}", e);
            break;
        }
        if let Err(e) = writer.flush().await {
            error!("Flush error: {}", e);
            break;
        }
    }
    debug!("Write loop exited");
}