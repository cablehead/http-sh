use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct Request {
    pub stamp: scru128::Scru128Id,
    pub message: String,
    pub proto: String,
    #[serde(with = "http_serde::method")]
    pub method: http::method::Method,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_ip: Option<std::net::IpAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_port: Option<u16>,
    #[serde(with = "http_serde::header_map")]
    pub headers: http::header::HeaderMap,
    #[serde(with = "http_serde::uri")]
    pub uri: http::Uri,
    pub path: String,
    pub query: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Response>,
}

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
pub struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::HashMap<String, String>>,
}
