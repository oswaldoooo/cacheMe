#[derive(serde::Serialize,serde::Deserialize)]
pub struct ObjectMetaData {
    pub status_code: u16,          /*上游响应码 */
    pub headers: std::collections::HashMap<String,String>, /*上游响应头 */
    pub size: usize,               /*响应体大小 */
    pub method: String,            /*请求方法 */
    pub url_path: String,          /*url路径 */
    pub host: String,              /*请求host */
    //内部数据
    pub expire: i64,       /*过期时间 */
    pub last_ref_time: i64,   /*上次访问时间 */
    pub ref_count: i64,       /*访问次数 */
    pub first_time_ref: i64,  /*第1次访问时间 */
    pub storage_level: i8,    /*储存级别 */
    pub storage_path: String, /*储存路径 */
    pub cache_zone: u8,       /*cahce id,用于去对应的cache拿资源*/
}
