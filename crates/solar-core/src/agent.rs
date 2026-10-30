//! Shows a running Solar Agent's data instead of reading the meter a second time.
//!
//! A SlimmeLezer serves one telnet client reliably; with two, telegrams are cut short and both
//! readers lose most of them. While Solar Agent runs in solar mode, the tray therefore reads its
//! snapshot from Solar Agent's local settings service and opens no device connections itself.
//! The service only listens on 127.0.0.1 and needs the token from Solar Agent's
//! `settings.json`. Everything read here is size- and time-bounded and validated before use.

use crate::config::Appliance;
use crate::runtime::Snapshot;
use crate::service::pause;
use crate::simulation::Preset;
use crate::sungrow::io_error_code;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub const SETTINGS_INFO_ENV: &str = "SOLAR_AGENT_SETTINGS_INFO";
pub const DATA_DIRECTORY: &str = "Solar Agent Data";
pub const SETTINGS_INFO_FILE: &str = "settings.json";
pub const MAX_INFO_BYTES: u64 = 4 * 1024;
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
pub const TIMEOUT: Duration = Duration::from_millis(1500);
/// The longest a whole response may take, however slowly it trickles in.
pub const RESPONSE_DEADLINE: Duration = Duration::from_secs(3);
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long Solar Agent may be absent before the tray connects to the meter itself, so a
/// Solar Agent restart does not briefly put two clients on the meter.
pub const ABSENT_GRACE_MS: i64 = 10_000;
const TOKEN_LENGTH: usize = 48;
const MAX_CHUNK_LINE: usize = 64;
const MAX_MESSAGE_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// Solar Agent is not running or not reading a meter, so the tray may connect itself.
    Absent(String),
    /// Solar Agent is running, but the tray cannot use its answer. Solar Agent may still be
    /// reading the meter, so the tray must not connect itself.
    Unusable(String),
}

impl AgentError {
    pub fn message(&self) -> &str {
        match self {
            AgentError::Absent(message) | AgentError::Unusable(message) => message,
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for AgentError {}

/// Printable, single-line text of bounded length from an untrusted message.
fn clean(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(MAX_MESSAGE_CHARS).collect()
}

/// Where Solar Agent writes its local address and token: the `SOLAR_AGENT_SETTINGS_INFO` path
/// when set, otherwise `%LOCALAPPDATA%\Solar Agent Data\settings.json` (as `paths.ts` does).
pub fn settings_info_path(overridden: Option<OsString>, local_app_data: Option<&Path>) -> Option<PathBuf> {
    match overridden.filter(|path| !path.is_empty()) {
        Some(path) => Some(PathBuf::from(path)),
        None => local_app_data.map(|base| base.join(DATA_DIRECTORY).join(SETTINGS_INFO_FILE)),
    }
}

/// Solar Agent's local settings service. The token is never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    port: u16,
    token: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint").field("port", &self.port).field("token", &"<redacted>").finish()
    }
}

/// A control Solar Agent applies to its solar runtime, as its own menus do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// An appliance id or a simulation scenario.
    Scenario(String),
    /// A custom simulated reading in signed watts.
    Watts(i64),
}

impl Endpoint {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Parses `{"url": "http://127.0.0.1:<port>/#token=<48 hex digits>", "port": <port>}`. Only
    /// the port and token are used: requests always go to 127.0.0.1.
    pub fn parse(text: &str) -> Result<Endpoint, String> {
        let value: Value = serde_json::from_str(text).map_err(|_| "Solar Agent's settings.json is not valid JSON")?;
        let port = value
            .get("port")
            .and_then(Value::as_u64)
            .filter(|port| (1..=65535).contains(port))
            .ok_or("Solar Agent's settings.json has no valid port")?;
        let token = value
            .get("url")
            .and_then(Value::as_str)
            .and_then(|url| url.rsplit_once("#token="))
            .map(|(_, token)| token)
            .filter(|token| {
                token.len() == TOKEN_LENGTH && token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
            .ok_or("Solar Agent's settings.json has no valid token")?;
        Ok(Endpoint { port: port as u16, token: token.to_string() })
    }

    pub fn read(path: &Path) -> Result<Endpoint, AgentError> {
        let unusable = |message: String| AgentError::Unusable(message);
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(AgentError::Absent("Solar Agent has not started on this computer".into()));
            }
            Err(error) => return Err(unusable(format!("Cannot read Solar Agent's settings.json ({error})"))),
        };
        let mut text = String::new();
        file.take(MAX_INFO_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|error| unusable(format!("Cannot read Solar Agent's settings.json ({error})")))?;
        if text.len() as u64 > MAX_INFO_BYTES {
            return Err(unusable("Solar Agent's settings.json is too large".into()));
        }
        Endpoint::parse(&text).map_err(unusable)
    }

    /// Solar Agent's current solar data.
    pub fn status(&self) -> Result<AgentData, AgentError> {
        let (code, body) = self.request("GET", "/api/status", None)?;
        check(code, &body)?;
        parse_status(&body)
    }

    /// Applies a control and returns the solar data Solar Agent answers with.
    pub fn control(&self, control: &Control) -> Result<AgentData, AgentError> {
        let (path, body) = match control {
            Control::Scenario(scenario) => ("/api/companion/scenario", json!({ "scenario": scenario })),
            Control::Watts(watts) => ("/api/companion/simulation", json!({ "watts": watts })),
        };
        let (code, body) = self.request("POST", path, Some(&body))?;
        check(code, &body)?;
        parse_status(&body)
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Vec<u8>), AgentError> {
        let silent = |error: std::io::Error| {
            // An accepted connection is evidence that the agent may still own the meter.
            AgentError::Unusable(format!("Solar Agent did not answer ({})", io_error_code(&error)))
        };
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)
            .map_err(|error| AgentError::Absent(format!("Solar Agent is not running ({})", io_error_code(&error))))?;
        stream.set_read_timeout(Some(TIMEOUT)).map_err(silent)?;
        stream.set_write_timeout(Some(TIMEOUT)).map_err(silent)?;
        let _ = stream.set_nodelay(true);
        let body = body.map(Value::to_string);
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nx-companion-token: {}\r\nAccept: application/json\r\nConnection: close\r\n",
            self.port, self.token
        );
        if let Some(body) = &body {
            request.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        request.push_str(body.as_deref().unwrap_or(""));
        stream.write_all(request.as_bytes()).map_err(silent)?;
        read_response(&mut stream)
    }
}

/// Maps a non-200 answer to an error, using the service's `{"error": ...}` message when present.
fn check(code: u16, body: &[u8]) -> Result<(), AgentError> {
    match code {
        200 => Ok(()),
        401 => Err(AgentError::Unusable("Solar Agent did not accept the token in its settings.json".into())),
        _ => {
            let message = serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|value| value.get("error").and_then(Value::as_str).map(clean))
                .filter(|message| !message.trim().is_empty());
            Err(AgentError::Unusable(match message {
                Some(message) => format!("Solar Agent: {message}"),
                None => format!("Solar Agent answered HTTP {code}"),
            }))
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

enum Framing {
    Length(usize),
    Chunked,
    Close,
}

fn parse_head(head: &[u8]) -> Result<(u16, Framing), &'static str> {
    let head = std::str::from_utf8(head).map_err(|_| "headers are not text")?;
    let mut lines = head.split("\r\n");
    let mut status = lines.next().unwrap_or_default().splitn(3, ' ');
    if !matches!(status.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
        return Err("not an HTTP/1.x response");
    }
    let code = status
        .next()
        .filter(|code| code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|code| (200..=599).contains(code))
        .ok_or("invalid status code")?;
    let (mut length, mut chunked) = (None, false);
    for line in lines {
        let (name, value) = line.split_once(':').ok_or("malformed header")?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("invalid Content-Length");
            }
            let value = value.parse::<usize>().map_err(|_| "invalid Content-Length")?;
            if length.is_some_and(|known| known != value) {
                return Err("conflicting Content-Length");
            }
            length = Some(value);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if !value.eq_ignore_ascii_case("chunked") {
                return Err("unsupported Transfer-Encoding");
            }
            chunked = true;
        }
    }
    match (chunked, length) {
        (true, Some(_)) => Err("both Content-Length and chunked"),
        (true, None) => Ok((code, Framing::Chunked)),
        (false, Some(length)) if length > MAX_BODY_BYTES => Err("body too large"),
        (false, Some(length)) => Ok((code, Framing::Length(length))),
        (false, None) => Ok((code, Framing::Close)),
    }
}

/// Decodes `Transfer-Encoding: chunked` as bytes arrive; `done` once the last chunk is seen.
#[derive(Default)]
struct Chunks {
    position: usize,
    body: Vec<u8>,
    done: bool,
}

impl Chunks {
    fn advance(&mut self, raw: &[u8]) -> Result<(), &'static str> {
        while !self.done {
            let rest = &raw[self.position..];
            let Some(line_end) = find(rest, b"\r\n") else {
                return if rest.len() > MAX_CHUNK_LINE { Err("invalid chunk size") } else { Ok(()) };
            };
            let digits = std::str::from_utf8(&rest[..line_end.min(MAX_CHUNK_LINE + 1)])
                .ok()
                .filter(|_| line_end <= MAX_CHUNK_LINE)
                .and_then(|line| line.split(';').next())
                .map(str::trim)
                .filter(|digits| (1..=8).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or("invalid chunk size")?;
            let size = usize::from_str_radix(digits, 16).map_err(|_| "invalid chunk size")?;
            if size == 0 {
                self.done = true;
                return Ok(());
            }
            if self.body.len() + size > MAX_BODY_BYTES {
                return Err("body too large");
            }
            let start = line_end + 2;
            let Some(data) = rest.get(start..start + size) else { return Ok(()) };
            match rest.get(start + size..start + size + 2) {
                None => return Ok(()),
                Some([b'\r', b'\n']) => {}
                Some(_) => return Err("chunk is not terminated"),
            }
            self.body.extend_from_slice(data);
            self.position += start + size + 2;
        }
        Ok(())
    }
}

/// Reads one response: the status code and the decoded body.
fn read_response(stream: &mut impl Read) -> Result<(u16, Vec<u8>), AgentError> {
    let invalid = |message: &str| AgentError::Unusable(format!("Solar Agent sent an invalid response ({message})"));
    let deadline = Instant::now() + RESPONSE_DEADLINE;
    let mut raw = Vec::new();
    let mut head: Option<(u16, Framing, usize)> = None;
    let mut chunks = Chunks::default();
    let mut eof = false;
    loop {
        if head.is_none() {
            if let Some(end) = find(&raw, b"\r\n\r\n") {
                let (code, framing) = parse_head(&raw[..end]).map_err(invalid)?;
                head = Some((code, framing, end + 4));
            } else if raw.len() > MAX_HEADER_BYTES {
                return Err(invalid("headers too large"));
            } else if eof {
                return Err(invalid("no complete headers"));
            }
        }
        if let Some((code, framing, start)) = &head {
            let body = &raw[*start..];
            let complete = match framing {
                Framing::Length(length) => (body.len() >= *length).then(|| body[..*length].to_vec()),
                Framing::Close if body.len() > MAX_BODY_BYTES => return Err(invalid("body too large")),
                Framing::Close => eof.then(|| body.to_vec()),
                Framing::Chunked => {
                    chunks.advance(body).map_err(invalid)?;
                    chunks.done.then(|| std::mem::take(&mut chunks.body))
                }
            };
            if let Some(body) = complete {
                return Ok((*code, body));
            }
            if eof {
                return Err(invalid("truncated body"));
            }
            if raw.len() > MAX_HEADER_BYTES + 2 * MAX_BODY_BYTES {
                return Err(invalid("body too large"));
            }
        }
        if Instant::now() >= deadline {
            return Err(AgentError::Unusable("Solar Agent did not answer in time".into()));
        }
        let mut buffer = [0u8; 8 * 1024];
        match stream.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(n) => raw.extend_from_slice(&buffer[..n]),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(AgentError::Unusable(format!("Solar Agent did not answer ({})", io_error_code(&error))));
            }
        }
    }
}

/// Solar Agent's solar data, ready for the tray.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentData {
    /// The snapshot, with the simulation presets and the meter and inverter addresses filled in.
    pub snapshot: Snapshot,
    /// The appliances to choose from, in Solar Agent's order.
    pub appliances: IndexMap<String, Appliance>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireAppliance {
    name: String,
    peak_w: f64,
}

#[derive(Deserialize)]
struct WireSimulation {
    #[serde(default)]
    presets: Vec<Preset>,
}

fn valid_host(host: &str) -> bool {
    (1..=253).contains(&host.len()) && host.bytes().all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
}

/// Parses a `GET /api/status` (or control) answer. Only the known non-solar `agents` mode is
/// [`AgentError::Absent`]. Missing solar data does not prove that the meter has been released.
pub fn parse_status(body: &[u8]) -> Result<AgentData, AgentError> {
    let unusable = |message: String| AgentError::Unusable(clean(&message));
    let status: Value =
        serde_json::from_slice(body).map_err(|error| unusable(format!("Solar Agent status is not JSON ({error})")))?;
    let companion = status
        .get("companion")
        .filter(|companion| companion.is_object())
        .ok_or_else(|| unusable("Solar Agent status has no companion section".into()))?;
    match companion.get("mode").and_then(Value::as_str) {
        Some("solar") => {}
        Some("agents") => {
            return Err(AgentError::Absent("Solar Agent is not in solar mode".into()));
        }
        _ => return Err(unusable("Solar Agent status has no valid companion mode".into())),
    }
    let raw = match companion.get("snapshot") {
        None | Some(Value::Null) => return Err(unusable("Solar Agent has no solar data yet".into())),
        Some(raw) => raw,
    };
    let unreadable =
        |error: serde_json::Error| unusable(format!("Solar Agent sent data this version cannot read ({error})"));
    let mut snapshot = Snapshot::deserialize(raw).map_err(unreadable)?;
    if let Some(simulation) = raw.get("simulation").filter(|value| !value.is_null()) {
        snapshot.presets = WireSimulation::deserialize(simulation).map_err(unreadable)?.presets;
    }
    let appliances = match raw.get("appliances") {
        Some(value) => IndexMap::<String, WireAppliance>::deserialize(value).map_err(unreadable)?,
        None => IndexMap::new(),
    };
    let appliances = appliances
        .into_iter()
        .filter(|(_, appliance)| appliance.peak_w.is_finite())
        .map(|(id, appliance)| (id, Appliance { name: appliance.name, peak_w: appliance.peak_w }))
        .collect();
    let solar = companion.get("solar");
    let address = |section: &str| {
        let source = solar?.get(section)?;
        let host = source.get("host")?.as_str().filter(|host| valid_host(host))?;
        let port = source.get("port")?.as_u64().filter(|port| (1..=65535).contains(port))?;
        Some(format!("{host}:{port}"))
    };
    if snapshot.simulated {
        snapshot.meter_address = None;
        snapshot.inverter_address = None;
    } else {
        snapshot.meter_address = Some(address("energySource").unwrap_or_else(|| "address unknown".into()));
        snapshot.inverter_address = address("inverterSource");
    }
    Ok(AgentData { snapshot, appliances })
}

pub type AgentResult = Result<Arc<AgentData>, AgentError>;

fn fetch(info_path: Option<&Path>, control: Option<&Control>) -> Result<AgentData, AgentError> {
    let path = info_path.ok_or_else(|| AgentError::Absent("Solar Agent's settings folder is unknown".into()))?;
    let endpoint = Endpoint::read(path)?;
    match control {
        Some(control) => endpoint.control(control),
        None => endpoint.status(),
    }
}

struct Latest {
    ticket: u64,
    result: AgentResult,
}

fn lock(latest: &Mutex<Latest>) -> MutexGuard<'_, Latest> {
    latest.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keeps the newest answer; an older request that finishes later does not overwrite it.
fn store(latest: &Mutex<Latest>, ticket: u64, result: AgentResult) {
    let mut latest = lock(latest);
    if ticket >= latest.ticket {
        *latest = Latest { ticket, result };
    }
}

/// Polls Solar Agent on a background thread; the newest answer is read without waiting.
pub struct AgentLink {
    info_path: Option<PathBuf>,
    latest: Arc<Mutex<Latest>>,
    tickets: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl AgentLink {
    /// Asks Solar Agent once before returning, then every [`POLL_INTERVAL`].
    pub fn start(info_path: Option<PathBuf>) -> Self {
        let first = fetch(info_path.as_deref(), None).map(Arc::new);
        let latest = Arc::new(Mutex::new(Latest { ticket: 0, result: first }));
        let tickets = Arc::new(AtomicU64::new(1));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (path, latest, tickets, stop) = (info_path.clone(), latest.clone(), tickets.clone(), stop.clone());
            thread::Builder::new()
                .name("solar-agent".into())
                .spawn(move || {
                    while pause(&stop, POLL_INTERVAL) {
                        let ticket = tickets.fetch_add(1, Ordering::Relaxed);
                        let result = fetch(path.as_deref(), None).map(Arc::new);
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        store(&latest, ticket, result);
                    }
                })
                .expect("spawn worker thread");
        }
        AgentLink { info_path, latest, tickets, stop }
    }

    pub fn latest(&self) -> AgentResult {
        lock(&self.latest).result.clone()
    }

    /// Sends a control to Solar Agent and keeps the data it answers with.
    pub fn control(&self, control: &Control) -> Result<(), AgentError> {
        let ticket = self.tickets.fetch_add(1, Ordering::Relaxed);
        let data = fetch(self.info_path.as_deref(), Some(control))?;
        store(&self.latest, ticket, Ok(Arc::new(data)));
        Ok(())
    }
}

impl Drop for AgentLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Solar Agent's data; the tray opens no device connections.
    Agent,
    /// The tray's own meter and inverter connections.
    Direct,
}

/// Picks where the tray's data comes from. Evidence that Solar Agent may own the meter keeps
/// the tray off it, even without usable data; the tray connects only after Solar Agent has been absent
/// for [`ABSENT_GRACE_MS`], or right away when it is absent at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceChooser {
    source: Source,
    absent_since: Option<i64>,
}

impl SourceChooser {
    pub fn new<T>(first: &Result<T, AgentError>) -> Self {
        let source = if matches!(first, Err(AgentError::Absent(_))) { Source::Direct } else { Source::Agent };
        SourceChooser { source, absent_since: None }
    }

    pub fn source(&self) -> Source {
        self.source
    }

    /// `now` is a monotonic time in milliseconds.
    pub fn update<T>(&mut self, latest: &Result<T, AgentError>, now: i64) -> Source {
        match latest {
            Err(AgentError::Absent(_)) if self.source == Source::Agent => {
                let since = *self.absent_since.get_or_insert(now);
                if now - since >= ABSENT_GRACE_MS {
                    self.source = Source::Direct;
                    self.absent_since = None;
                }
            }
            Err(AgentError::Absent(_)) => {}
            _ => {
                self.source = Source::Agent;
                self.absent_since = None;
            }
        }
        self.source
    }
}
