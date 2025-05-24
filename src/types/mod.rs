use std::pin::Pin;

pub mod metadata;

pub type Hash = String;
pub struct ObjectId {}
pub struct ReqOption {
    pub range: Option<(i64, i64)>,
    pub method: String,
    pub request_time: i64, /*接收到请求时的时间戳 */
}

pub struct Request {
    pub host: String,
    pub method: String,
    pub url_path: String,
    pub headers: hyper::HeaderMap,
    pub timestamp: i64,
}
pub struct Response {
    pub status_code: u16,
    pub headers: hyper::HeaderMap,
    pub content: Pin<Box<dyn tokio::io::AsyncRead + Send + Sync>>,
}

