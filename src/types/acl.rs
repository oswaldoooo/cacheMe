use serde_json::Value;

#[derive(Clone, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum Acl {
    Proxy(Proxy),
    Cache(Cache),
}
// impl TryFrom<Value> for Acl {
//     type Error = String;

//     fn try_from(value: Value) -> Result<Self, Self::Error> {
//         let kind = value.get("kind")
//             .and_then(Value::as_str)
//             .ok_or("Missing or invalid 'kind' field")?;

//         match kind {
//             "proxy" => {
//                 let proxy: Proxy = serde_json::from_value(value)
//                     .map_err(|e| format!("Failed to parse Proxy: {e}"))?;
//                 Ok(Acl::Proxy(proxy))
//             },
//             "cache" => {
//                 let cache: Cache = serde_json::from_value(value)
//                     .map_err(|e| format!("Failed to parse Cache: {e}"))?;
//                 Ok(Acl::Cache(cache))
//             },
//             _ => Err(format!("Unknown kind: {kind}")),
//         }
//     }
// }
#[derive(serde::Deserialize, Clone)]
pub struct Proxy {
    pub host: String,
    pub path_match: String,
    pub allow_method: Option<Vec<String>>,
}
#[derive(serde::Deserialize, Clone)]
pub struct Cache {
    pub host: String,
    pub path_match: String,
    pub allow_method: Option<Vec<String>>,
}

impl Acl {
    pub fn host(&self) -> &str {
        match self {
            Acl::Proxy(proxy) => proxy.host.as_str(),
            Acl::Cache(cache) => cache.host.as_str(),
        }
    }
    pub fn path_match(&self) -> &str {
        match self {
            Acl::Proxy(proxy) => proxy.path_match.as_str(),
            Acl::Cache(cache) => cache.path_match.as_str(),
        }
    }
    pub fn method_match(&self, method: &str) -> bool {
        match self {
            Acl::Proxy(proxy) => match &proxy.allow_method {
                Some(allow_method) => {
                    if allow_method.is_empty() {
                        return true;
                    }
                    for meth in allow_method.iter() {
                        if meth.as_str() == method {
                            return true;
                        }
                    }
                    false
                }
                None => true,
            },
            Acl::Cache(cache) => match &cache.allow_method {
                Some(allow_method) => {
                    for meth in allow_method.iter() {
                        if meth.as_str() == method {
                            return true;
                        }
                    }
                    false
                }
                None => true,
            },
        }
    }
}
