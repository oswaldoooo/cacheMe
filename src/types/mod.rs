use std::pin::Pin;

pub mod metadata;
pub mod acl;
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
impl Request {
    pub fn get_if_none_match(&self) -> Option<&str> {
        if let Some(etag) = self.headers.get("if-none-match") {
            if !etag.is_empty() {
                if let Ok(etag) = etag.to_str() {
                    return Some(etag);
                }
            }
        }
        None
    }
    pub fn get_if_modified_since(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        if let Some(if_modified_since) = self.headers.get("if-modified-since") {
            if !if_modified_since.is_empty() {
                if let Ok(if_modified_since) = if_modified_since.to_str() {
                    if let Ok(datetime) = chrono::DateTime::parse_from_rfc2822(if_modified_since) {
                        return Some(datetime.to_utc());
                    }
                }
            }
        }
        None
    }
}
pub struct Response {
    pub status_code: u16,
    pub headers: hyper::HeaderMap,
    pub content: Pin<Box<dyn tokio::io::AsyncRead + Send + Sync>>,
}
