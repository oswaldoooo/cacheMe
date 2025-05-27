use std::pin::Pin;

#[cfg(feature="mp4")]
pub mod mp4;

pub mod acl;
pub mod metadata;
pub type Hash = String;
pub struct ObjectId {}
pub struct ReqOption {
    pub range: Option<(i64, i64)>,
    pub method: String,
    pub request_time: i64, /*接收到请求时的时间戳 */
}
#[cfg(feature="hyper")]
type HeaderMap=hyper::HeaderMap;
#[cfg(feature="actix")]
type HeaderMap=actix_http::header::HeaderMap;
pub struct Request {
    pub host: String,
    pub method: String,
    pub url_path: String,
    pub headers: HeaderMap,
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
    pub fn get_accept_encoding(&self) -> Option<Vec<&str>> {
        let ae = self
            .headers
            .get_all("accept-encoding")
            .into_iter()
            .filter_map(|item| {
                if let Ok(item) = item.to_str() {
                    return Some(item);
                }
                None
            })
            .collect::<Vec<&str>>();
        if ae.is_empty() {
            None
        } else {
            Some(ae)
        }
    }
}
pub struct Response {
    pub status_code: u16,
    pub headers: hyper::HeaderMap,
    pub content: Content,
}
pub enum Content{
    Stream(Pin<Box<dyn tokio::io::AsyncRead + Send+Sync>>),
    Path(String),
}