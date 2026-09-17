//! A small client for the mihomo RESTful controller.
//!
//! Only the endpoints the tray needs are implemented, see
//! <https://wiki.metacubex.one/api/>.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use async_io::Timer;
use async_web_client::prelude::*;
use async_web_client::{HttpError, ResponseBody, TransportError};
use futures_lite::future;
use http::{Method, Request, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The controller runs next to us, so give up quickly when it is not there.
const TIMEOUT: Duration = Duration::from_secs(3);

type Result<T> = std::result::Result<T, Error>;

/// Client for the kernel's [external controller].
///
/// [external controller]: https://wiki.metacubex.one/config/general/#external-controller
pub struct Api {
    base: String,
    secret: Option<String>,
}

impl Api {
    /// Talk to `address`, either `host:port` or a full `http://` URL.
    pub fn new(address: &str, secret: Option<String>) -> Self {
        let base = if address.starts_with("http") {
            address.trim_end_matches('/').to_owned()
        } else {
            format!("http://{address}")
        };
        Self { base, secret }
    }

    /// Read everything the tray displays.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        #[derive(Deserialize)]
        struct Configs {
            mode: Mode,
        }
        #[derive(Deserialize)]
        struct Version {
            version: String,
        }
        let Configs { mode } = self.get("/configs").await?;
        let Proxies { proxies } = self.get("/proxies").await?;
        let Version { version } = self.get("/version").await?;
        let Rules { rules } = self.get("/rules").await?;

        Ok(Snapshot {
            version,
            mode,
            groups: groups(&proxies),
            rules,
            error: None,
        })
    }

    /// Carry out a command picked from the menu.
    pub async fn run(&self, command: Command) -> Result<()> {
        let (method, path, body) = match command {
            // Nothing to send, this command only asks for a new snapshot.
            Command::Refresh => return Ok(()),
            Command::SetMode(mode) => (
                Method::PATCH,
                "/configs".to_owned(),
                Some(json!({ "mode": mode })),
            ),
            Command::Select { group, node } => (
                Method::PUT,
                format!("/proxies/{}", encode(&group)),
                Some(json!({ "name": node })),
            ),
            // `/rules/disable` takes an object keyed by rule index.
            Command::SetRuleDisabled { index, disabled } => (
                Method::PATCH,
                "/rules/disable".to_owned(),
                Some(json!({ index.to_string(): disabled })),
            ),
            Command::ReloadConfig => (
                Method::PUT,
                "/configs?force=true".to_owned(),
                Some(json!({ "path": "", "payload": "" })),
            ),
            Command::UpdateGeo => (Method::POST, "/upgrade/geo".to_owned(), None),
            Command::FlushFakeIp => (Method::POST, "/cache/fakeip/flush".to_owned(), None),
            Command::FlushDns => (Method::POST, "/cache/dns/flush".to_owned(), None),
            Command::Restart => (
                Method::POST,
                "/restart".to_owned(),
                Some(json!({ "path": "", "payload": "" })),
            ),
        };
        self.send(method, &path, body).await.map(drop)
    }

    /// `GET` and decode a JSON document.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut response = self.send(Method::GET, path, None).await?;
        Ok(response.body_mut().json(None).await?)
    }

    /// Send a request, failing on an error status and after [`TIMEOUT`].
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<http::Response<ResponseBody>> {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base));
        if let Some(secret) = &self.secret {
            request = request.header(http::header::AUTHORIZATION, format!("Bearer {secret}"));
        }
        let body = match body {
            Some(body) => {
                request = request.header(http::header::CONTENT_TYPE, "application/json");
                body.to_string().into_bytes()
            }
            None => Vec::new(),
        };

        let response = future::or(
            async { request.body(body)?.send().await.map_err(Error::from) },
            async {
                Timer::after(TIMEOUT).await;
                Err(Error::Timeout(TIMEOUT))
            },
        )
        .await?;

        match response.status().is_success() {
            true => Ok(response),
            false => Err(Error::Status(response.status())),
        }
    }
}

/// Something the user picked in the menu.
#[derive(Clone)]
pub enum Command {
    /// Re-read the kernel without changing anything, e.g. before showing the menu.
    Refresh,
    SetMode(Mode),
    Select {
        group: String,
        node: String,
    },
    /// Switch a rule off, or back on.
    SetRuleDisabled {
        index: u64,
        disabled: bool,
    },
    ReloadConfig,
    UpdateGeo,
    FlushFakeIp,
    FlushDns,
    Restart,
}

/// Everything the tray knows about the running kernel.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub version: String,
    pub mode: Mode,
    pub groups: Vec<Group>,
    pub rules: Vec<Rule>,
    /// Set while the controller cannot be reached, or after a failed command.
    pub error: Option<String>,
}

impl Snapshot {
    /// Read the kernel state, describing the failure in [`Snapshot::error`]
    /// when it cannot be reached.
    pub async fn fetch(api: &Api) -> Self {
        api.snapshot().await.unwrap_or_else(|error| Self {
            error: Some(error.to_string()),
            ..Self::default()
        })
    }
}

/// The kernel's `mode`.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Rule,
    Direct,
    Global,
}

impl Mode {
    /// The modes in menu order.
    pub const ALL: [Self; 3] = [Self::Rule, Self::Direct, Self::Global];

    /// Name shown in the menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::Rule => "Rule",
            Self::Direct => "Direct",
            Self::Global => "Global",
        }
    }
}

/// A policy group, e.g. a `Selector` or a `URLTest`.
#[derive(Clone)]
pub struct Group {
    pub name: String,
    /// Node the group currently uses.
    pub now: Option<String>,
    pub nodes: Vec<String>,
}

impl Group {
    /// Index of [`Group::now`], for the radio group of the menu.
    pub fn selection(&self) -> usize {
        self.now
            .as_ref()
            .and_then(|now| self.nodes.iter().position(|node| node == now))
            .unwrap_or_default()
    }
}

/// A rule of the running configuration.
#[derive(Clone, Deserialize)]
pub struct Rule {
    /// Position in the rule list, which `/rules/disable` takes.
    pub index: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: String,
    pub proxy: String,
    /// Runtime state; the kernel only reports it for rules it can switch off.
    #[serde(default, deserialize_with = "null_default")]
    pub extra: RuleExtra,
}

/// What the kernel knows about a rule beyond its definition.
#[derive(Clone, Default, Deserialize)]
pub struct RuleExtra {
    pub disabled: bool,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            kind,
            payload,
            proxy,
            ..
        } = self;
        if payload.is_empty() {
            write!(f, "{kind} → {proxy}")
        } else {
            write!(f, "{kind} {payload} → {proxy}")
        }
    }
}

/// Anything that can go wrong while talking to the controller.
///
/// The tray only ever shows this as one menu entry, so the message is all that
/// is kept.
#[derive(Debug)]
pub enum Error {
    /// The request could not be built, sent or read.
    Transport(String),
    /// The kernel answered with an error status.
    Status(StatusCode),
    /// The kernel did not answer in time.
    Timeout(Duration),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => f.write_str(message),
            Self::Status(status) => write!(f, "controller answered HTTP {status}"),
            Self::Timeout(timeout) => {
                write!(f, "controller did not answer within {}s", timeout.as_secs())
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<http::Error> for Error {
    fn from(error: http::Error) -> Self {
        Self::Transport(error.to_string())
    }
}

impl From<HttpError> for Error {
    fn from(error: HttpError) -> Self {
        // The crate prints the underlying `io::Error` with `{:?}`, which is far
        // too noisy for a menu entry; the error itself says it plainly, e.g.
        // "Connection refused (os error 111)".
        Self::Transport(match error {
            HttpError::ConnectError(TransportError::TcpConnect(io))
            | HttpError::ConnectError(TransportError::TlsConnect(io)) => io.to_string(),
            error => error.to_string(),
        })
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Transport(error.to_string())
    }
}

/// The response of `/proxies`.
#[derive(Deserialize)]
struct Proxies {
    proxies: HashMap<String, Proxy>,
}

/// The response of `/rules`.
#[derive(Deserialize)]
struct Rules {
    rules: Vec<Rule>,
}

/// A proxy or a policy group, as reported by `/proxies`.
#[derive(Deserialize)]
struct Proxy {
    now: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default, deserialize_with = "null_default")]
    all: Vec<String>,
}

/// Keep the visible policy groups with their members.
fn groups(proxies: &HashMap<String, Proxy>) -> Vec<Group> {
    let mut groups: Vec<Group> = proxies
        .iter()
        .filter(|(_, proxy)| !proxy.all.is_empty() && !proxy.hidden)
        .map(|(name, proxy)| Group {
            name: name.clone(),
            now: proxy.now.clone(),
            nodes: proxy.all.clone(),
        })
        .collect();
    // The kernel sends the proxies as a JSON object, so restore a stable order.
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    groups
}

/// Deserialize `null` (the kernel uses it for "not measured yet") like a
/// missing field.
fn null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::deserialize(deserializer)?.unwrap_or_default())
}

/// Percent-encode a path segment or a query value.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
