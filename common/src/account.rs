use std::time::Duration;
use crate::vault::Vault;

pub struct Account {
    pub label: String,
    pub username: Option<String>,
    pub passwordeval: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls: Option<bool>,
    pub default: bool,
    pub password: Option<Vec<u8>>,
    pub vault: Vault,
    pub timeout: Duration,
    pub cert_root: Option<String>,
}
