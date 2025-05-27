use std::io::Write;
use std::pin::Pin;

use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;

use crate::ErrorKind;
pub async fn build_gzip_encoder_stream<T: AsyncRead + Send + Unpin>(
    mut src: T,
    data_len: Option<&mut usize>,
) -> Result<Pin<Box<dyn AsyncRead + Send + Sync>>, ErrorKind> {
    let mut data = Vec::new();
    let mut encoder = flate2::write::GzEncoder::new(&mut data, flate2::Compression::best());
    let mut tempbuff = [0u8; 1 << 20];
    loop {
        let size = src.read(&mut tempbuff).await.map_err(|err| {
            ErrorKind::OperateError(format!(
                "read source stream when build gzip stream failed {err}"
            ))
        })?;
        if size < 1 {
            break;
        }
        encoder.write_all(&tempbuff[..size]).map_err(|err| {
            ErrorKind::OperateError(format!("gzip stream write data failed {err}"))
        })?;
    }
    encoder
        .finish()
        .map_err(|err| ErrorKind::OperateError(format!("finished gzip stream failed {err}")))?;
    if let Some(data_len) = data_len {
        *data_len = data.len();
    }
    Ok(Box::pin(tokio::io::BufReader::new(std::io::Cursor::new(
        data,
    ))))
}

pub struct ChunkWriter {
    boundary: String,
    data: Vec<u8>,
}

impl ChunkWriter {
    pub async fn write_chunk(&mut self, data: &[u8]) {
        let _ = self
            .data
            .write_all(format!("{:x}\r\n", data.len()).as_bytes());
        self.data.copy_from_slice(data);
        self.data.push(b'\r');
        self.data.push(b'\n');
    }
    pub async fn finalize(&mut self) -> &[u8] {
        self.data.copy_from_slice(b"0\r\n\r\n");
        &self.data
    }
}
