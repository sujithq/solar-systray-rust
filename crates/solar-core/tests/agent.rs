//! Reading a running Solar Agent: its settings info, its HTTP answers (against a fake local
//! server), the status payload, and when the tray reads the meter itself instead.

use solar_core::agent::{
    ABSENT_GRACE_MS, AgentError, AgentLink, Control, Endpoint, Source, SourceChooser, parse_status, settings_info_path,
};
use solar_core::decision::{Level, State};
use solar_core::present::{solar_info, tooltip};
use solar_core::service::SolarService;
use solar_core::sungrow::InverterStatus;
use solar_core::{Snapshot, SolarConfig};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn endpoint(port: u16) -> Endpoint {
    Endpoint::parse(&format!(r#"{{"url":"http://127.0.0.1:{port}/#token={TOKEN}","port":{port}}}"#)).unwrap()
}

fn unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "solar-core-agent-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Reads one request: its head and, when it has a Content-Length, its body.
fn read_request(socket: &mut TcpStream) -> String {
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map_or(0, |value| value.trim().parse::<usize>().unwrap());
            if raw.len() >= end + 4 + length {
                return String::from_utf8_lossy(&raw).into_owned();
            }
        }
        match socket.read(&mut buffer) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&raw).into_owned(),
            Ok(n) => raw.extend_from_slice(&buffer[..n]),
        }
    }
}

/// Serves one connection: sends `parts` with short pauses, then (unless `hold`) closes.
/// With `hold`, it keeps the connection open until the client closes it, so a client that
/// waits for the end of the stream instead of the end of the body would time out.
fn serve_once(parts: Vec<Vec<u8>>, hold: bool) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let _ = sender.send(read_request(&mut socket));
        for part in parts {
            if socket.write_all(&part).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(15));
        }
        if hold {
            let mut sink = [0u8; 64];
            let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
            while matches!(socket.read(&mut sink), Ok(n) if n > 0) {}
        }
    });
    (port, receiver)
}

fn response(code: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut raw = format!("HTTP/1.1 {code}\r\n{headers}\r\n").into_bytes();
    raw.extend_from_slice(body);
    raw
}

fn chunked(body: &[u8], size: usize) -> Vec<u8> {
    let mut raw = Vec::new();
    for chunk in body.chunks(size) {
        raw.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        raw.extend_from_slice(chunk);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");
    raw
}

fn unusable(result: Result<impl std::fmt::Debug, AgentError>) -> String {
    match result {
        Err(AgentError::Unusable(message)) => message,
        other => panic!("expected Unusable, got {other:?}"),
    }
}

fn absent(result: Result<impl std::fmt::Debug, AgentError>) -> String {
    match result {
        Err(AgentError::Absent(message)) => message,
        other => panic!("expected Absent, got {other:?}"),
    }
}

#[test]
fn settings_info_path_prefers_the_override() {
    let local = Path::new(r"C:\Users\someone\AppData\Local");
    assert_eq!(settings_info_path(None, Some(local)), Some(local.join("Solar Agent Data").join("settings.json")));
    assert_eq!(
        settings_info_path(Some(OsString::from(r"D:\agent\settings.json")), Some(local)),
        Some(PathBuf::from(r"D:\agent\settings.json"))
    );
    assert_eq!(
        settings_info_path(Some(OsString::new()), Some(local)),
        Some(local.join("Solar Agent Data").join("settings.json"))
    );
    assert_eq!(settings_info_path(None, None), None);
}

#[test]
fn endpoint_parses_solar_agent_settings_info() {
    let endpoint = endpoint(4767);
    assert_eq!(endpoint.port(), 4767);
    let debug = format!("{endpoint:?}");
    assert!(!debug.contains(TOKEN) && debug.contains("redacted"), "{debug}");
}

#[test]
fn endpoint_rejects_malformed_settings_info() {
    let url = |port: &str, token: &str| format!(r#"{{"url":"http://127.0.0.1:4767/#token={token}","port":{port}}}"#);
    let short = &TOKEN[1..];
    let upper = TOKEN.to_ascii_uppercase();
    let cases = [
        "".to_string(),
        "not json".to_string(),
        "[]".to_string(),
        r#"{"port":4767}"#.to_string(),
        format!(r#"{{"url":"http://127.0.0.1:4767/#token={TOKEN}"}}"#),
        url("0", TOKEN),
        url("65536", TOKEN),
        url("-1", TOKEN),
        url("4767.5", TOKEN),
        url("\"4767\"", TOKEN),
        url("4767", short),
        url("4767", &upper),
        url("4767", &format!("{TOKEN}0")),
        url("4767", &format!("{}g", &TOKEN[1..])),
        format!(r#"{{"url":"http://127.0.0.1:4767/?token={TOKEN}","port":4767}}"#),
        format!(r#"{{"url":"http://127.0.0.1:4767/#token={TOKEN}\n","port":4767}}"#),
    ];
    for case in cases {
        assert!(Endpoint::parse(&case).is_err(), "accepted {case:?}");
    }
}

#[test]
fn endpoint_read_only_treats_missing_files_as_absent() {
    let dir = temp_dir();
    let message = absent(Endpoint::read(&dir.join("settings.json")));
    assert!(message.contains("has not started"), "{message}");

    let oversized = dir.join("oversized.json");
    std::fs::write(&oversized, " ".repeat(8 * 1024)).unwrap();
    assert!(unusable(Endpoint::read(&oversized)).contains("too large"));

    let invalid = dir.join("invalid.json");
    std::fs::write(&invalid, r#"{"url":"http://127.0.0.1:1/","port":1}"#).unwrap();
    assert!(unusable(Endpoint::read(&invalid)).contains("token"));

    let valid = dir.join("valid.json");
    std::fs::write(&valid, format!(r#"{{"url":"http://127.0.0.1:4767/#token={TOKEN}","port":4767}}"#)).unwrap();
    assert_eq!(Endpoint::read(&valid).unwrap(), endpoint(4767));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn parses_a_live_solar_agent_status() {
    let data = parse_status(fixture("agent-status.json").as_bytes()).unwrap();
    let s = &data.snapshot;
    assert_eq!(s.evaluation.appliance, "dishwasher");
    assert_eq!(s.evaluation.state, State::Importing);
    assert_eq!(s.evaluation.level, Level::Night);
    assert_eq!(s.evaluation.grid_import_w, Some(1250.0));
    assert_eq!(s.evaluation.reason, "Grid electricity is being imported");
    assert!(!s.simulated);
    assert!(s.presets.is_empty());
    assert_eq!(s.peak_forecast.projected_w, Some(1612.7777777777778));
    assert_eq!(s.daily_energy.export_wh, Some(8400));
    assert_eq!(s.diagnostics.checksum.as_deref(), Some("verified"));
    assert_eq!(s.diagnostics.telegrams, 3600);
    assert_eq!(s.inverter.status, InverterStatus::Online);
    assert_eq!(s.meter_address.as_deref(), Some("192.0.2.10:23"));
    assert_eq!(s.inverter_address.as_deref(), Some("192.0.2.30:502"));
    assert_eq!(
        data.appliances.keys().collect::<Vec<_>>(),
        ["dishwasher", "washingMachine", "heatPumpDryer", "conventionalDryer"]
    );
    assert_eq!(data.appliances["washingMachine"].peak_w, 2200.0);

    let meter = solar_info(s).section("Connections").unwrap().value("Meter").unwrap().to_string();
    assert!(meter.starts_with("192.0.2.10:23") && meter.ends_with("(live)"), "{meter}");
    assert!(tooltip(s).contains("Import 1250 W"), "{}", tooltip(s));
}

#[test]
fn parses_a_simulated_solar_agent_status() {
    let data = parse_status(fixture("agent-status-simulated.json").as_bytes()).unwrap();
    let s = &data.snapshot;
    assert!(s.simulated);
    assert_eq!(s.scenario.as_deref(), Some("good"));
    assert_eq!(s.simulated_w, Some(-2800));
    assert_eq!(s.presets.iter().map(|p| p.id.as_ref()).collect::<Vec<_>>(), ["excellent", "good", "importing"]);
    assert_eq!(s.presets[1].watts, -2800);
    assert_eq!(s.meter_address, None);
    assert_eq!(s.inverter_address, None);
    assert!(s.diagnostics.simulated && s.diagnostics.connected);
    assert_eq!(s.diagnostics.bytes, 0);
    assert_eq!(s.inverter.status, InverterStatus::Disabled);
    assert_eq!(data.appliances.keys().collect::<Vec<_>>(), ["washingMachine", "dishwasher"]);
}

fn edit_status(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut status: serde_json::Value = serde_json::from_str(&fixture("agent-status.json")).unwrap();
    edit(&mut status);
    status.to_string().into_bytes()
}

#[test]
fn only_an_explicit_other_mode_means_solar_agent_is_not_reading_the_meter() {
    let other_mode = edit_status(|s| s["companion"]["mode"] = "agents".into());
    assert!(absent(parse_status(&other_mode)).contains("not in solar mode"));
    let no_mode = edit_status(|s| s["companion"].as_object_mut().unwrap().clear());
    assert!(unusable(parse_status(&no_mode)).contains("mode"));
    let no_snapshot = edit_status(|s| s["companion"]["snapshot"] = serde_json::Value::Null);
    assert!(unusable(parse_status(&no_snapshot)).contains("no solar data"));
}

#[test]
fn malformed_status_is_unusable() {
    assert!(unusable(parse_status(b"not json")).contains("not JSON"));
    assert!(unusable(parse_status(b"[]")).contains("no companion"));
    assert!(unusable(parse_status(br#"{"companion":"solar"}"#)).contains("no companion"));
    let cases = [
        edit_status(|s| s["companion"]["snapshot"]["state"] = "SOMETHING_NEW".into()),
        edit_status(|s| s["companion"]["snapshot"]["surplusW"] = "lots".into()),
        edit_status(|s| s["companion"]["snapshot"]["checkedAt"] = serde_json::Value::Null),
        edit_status(|s| s["companion"]["snapshot"]["appliances"]["dishwasher"]["peakW"] = "1500".into()),
        edit_status(|s| s["companion"]["snapshot"]["simulation"] = serde_json::json!({"presets": [{"id": 1}]})),
        edit_status(|s| s["companion"]["snapshot"] = serde_json::json!([])),
    ];
    for case in cases {
        let message = unusable(parse_status(&case));
        assert!(message.contains("cannot read"), "{message}");
        assert!(message.chars().count() <= 160 && !message.contains('\n'), "{message:?}");
    }
}

#[test]
fn status_tolerates_new_fields_and_odd_addresses() {
    let newer = edit_status(|s| {
        s["companion"]["solar"]["somethingNew"] = true.into();
        s["companion"]["snapshot"]["somethingNew"] = serde_json::json!({"a": 1});
        s["companion"]["snapshot"]["diagnostics"]["somethingNew"] = 1.into();
    });
    assert!(parse_status(&newer).is_ok());

    let odd = edit_status(|s| {
        s["companion"]["solar"]["energySource"]["host"] = "evil host\r\nX: y".into();
        s["companion"]["solar"]["inverterSource"]["host"] = "".into();
    });
    let data = parse_status(&odd).unwrap();
    assert_eq!(data.snapshot.meter_address.as_deref(), Some("address unknown"));
    assert_eq!(data.snapshot.inverter_address, None);

    let no_config = edit_status(|s| {
        s["companion"].as_object_mut().unwrap().remove("solar");
    });
    let data = parse_status(&no_config).unwrap();
    assert_eq!(data.snapshot.meter_address.as_deref(), Some("address unknown"));

    let no_appliances = edit_status(|s| {
        s["companion"]["snapshot"].as_object_mut().unwrap().remove("appliances");
    });
    assert!(parse_status(&no_appliances).unwrap().appliances.is_empty());
}

#[test]
fn reads_a_chunked_status_and_sends_the_token() {
    let body = fixture("agent-status.json").into_bytes();
    let head = response("200 OK", "Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n", b"");
    let raw = chunked(&body, 1000);
    // Split mid-chunk-header and mid-chunk to exercise reassembly across reads.
    let (a, b) = raw.split_at(1009);
    let (b, c) = b.split_at(500);
    let (port, request) = serve_once(vec![head, a.to_vec(), b.to_vec(), c.to_vec()], true);
    let data = endpoint(port).status().unwrap();
    assert_eq!(data.snapshot.evaluation.grid_import_w, Some(1250.0));
    let request = request.recv().unwrap();
    assert!(request.starts_with("GET /api/status HTTP/1.1\r\n"), "{request}");
    assert!(request.contains(&format!("\r\nHost: 127.0.0.1:{port}\r\n")), "{request}");
    assert!(request.contains(&format!("\r\nx-companion-token: {TOKEN}\r\n")), "{request}");
    assert!(request.contains("\r\nConnection: close\r\n"), "{request}");
    assert!(!request.to_ascii_lowercase().contains("origin:"), "{request}");
}

#[test]
fn reads_a_status_with_content_length_without_waiting_for_close() {
    let body = fixture("agent-status.json").into_bytes();
    let head = format!("Content-Length: {}\r\n", body.len());
    let (port, _) = serve_once(vec![response("200 OK", &head, &body)], true);
    let started = Instant::now();
    assert!(endpoint(port).status().is_ok());
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
}

#[test]
fn reads_a_status_delimited_by_close() {
    let body = fixture("agent-status.json").into_bytes();
    let mut raw = b"HTTP/1.0 200 OK\r\ncontent-type: application/json\r\n\r\n".to_vec();
    raw.extend_from_slice(&body);
    let (port, _) = serve_once(vec![raw], false);
    assert!(endpoint(port).status().is_ok());
}

#[test]
fn sends_controls_as_json() {
    let body = fixture("agent-status-simulated.json").into_bytes();
    let ok = response("200 OK", &format!("Content-Length: {}\r\n", body.len()), &body);

    let (port, request) = serve_once(vec![ok.clone()], false);
    let data = endpoint(port).control(&Control::Scenario("good".into())).unwrap();
    assert_eq!(data.snapshot.scenario.as_deref(), Some("good"));
    let request = request.recv().unwrap();
    assert!(request.starts_with("POST /api/companion/scenario HTTP/1.1\r\n"), "{request}");
    assert!(request.contains("\r\nContent-Type: application/json\r\n"), "{request}");
    assert!(request.ends_with("\r\n\r\n{\"scenario\":\"good\"}"), "{request}");

    let (port, request) = serve_once(vec![ok], false);
    endpoint(port).control(&Control::Watts(-2800)).unwrap();
    let request = request.recv().unwrap();
    assert!(request.starts_with("POST /api/companion/simulation HTTP/1.1\r\n"), "{request}");
    assert!(request.ends_with("\r\n\r\n{\"watts\":-2800}"), "{request}");

    let tricky = "a\"b\\c\r\nd";
    let (port, request) = serve_once(vec![response("400 Bad Request", "Content-Length: 2\r\n", b"{}")], false);
    let _ = endpoint(port).control(&Control::Scenario(tricky.into()));
    let request = request.recv().unwrap();
    let body = &request[request.find("\r\n\r\n").unwrap() + 4..];
    assert_eq!(serde_json::from_str::<serde_json::Value>(body).unwrap()["scenario"], tricky);
}

#[test]
fn error_answers_are_unusable_with_their_message() {
    let (port, _) = serve_once(
        vec![response("401 Unauthorized", "Content-Length: 26\r\n", br#"{"error":"Missing token."}"#)],
        false,
    );
    assert!(unusable(endpoint(port).status()).contains("did not accept the token"));

    let body = br#"{"error":"Unknown scenario.\r\nInjected"}"#;
    let (port, _) =
        serve_once(vec![response("400 Bad Request", &format!("Content-Length: {}\r\n", body.len()), body)], false);
    let message = unusable(endpoint(port).control(&Control::Scenario("nope".into())));
    assert_eq!(message, "Solar Agent: Unknown scenario.  Injected");

    let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(1000));
    let (port, _) =
        serve_once(vec![response("500 Oops", &format!("Content-Length: {}\r\n", long.len()), long.as_bytes())], false);
    assert!(unusable(endpoint(port).status()).chars().count() <= 200);

    let (port, _) = serve_once(vec![response("503 Service Unavailable", "Content-Length: 4\r\n", b"busy")], false);
    assert_eq!(unusable(endpoint(port).status()), "Solar Agent answered HTTP 503");
}

#[test]
fn malformed_responses_are_unusable() {
    let big = format!("Content-Length: {}\r\n", 2 * 1024 * 1024);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not an HTTP/1.x response", b"SSH-2.0-OpenSSH\r\n\r\n".to_vec()),
        ("not an HTTP/1.x response", b"HTTP/2 200\r\n\r\n{}".to_vec()),
        ("invalid status code", b"HTTP/1.1 2000 OK\r\n\r\n".to_vec()),
        ("invalid status code", b"HTTP/1.1 101 Switching\r\n\r\n".to_vec()),
        ("malformed header", b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n".to_vec()),
        ("invalid Content-Length", response("200 OK", "Content-Length: -5\r\n", b"")),
        ("conflicting Content-Length", response("200 OK", "Content-Length: 2\r\nContent-Length: 3\r\n", b"{}")),
        ("unsupported Transfer-Encoding", response("200 OK", "Transfer-Encoding: gzip, chunked\r\n", b"")),
        (
            "both Content-Length and chunked",
            response("200 OK", "Content-Length: 5\r\nTransfer-Encoding: chunked\r\n", b"0\r\n\r\n"),
        ),
        ("body too large", response("200 OK", &big, b"")),
        ("body too large", response("200 OK", "Transfer-Encoding: chunked\r\n", b"200000\r\n")),
        ("invalid chunk size", response("200 OK", "Transfer-Encoding: chunked\r\n", b"zz\r\n")),
        ("invalid chunk size", response("200 OK", "Transfer-Encoding: chunked\r\n", &[b'1'; 100])),
        ("chunk is not terminated", response("200 OK", "Transfer-Encoding: chunked\r\n", b"2\r\n{}XX0\r\n\r\n")),
        ("truncated body", response("200 OK", "Content-Length: 100\r\n", b"{}")),
        ("truncated body", response("200 OK", "Transfer-Encoding: chunked\r\n", b"2\r\n{}\r\n")),
        ("no complete headers", b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n".to_vec()),
        ("headers are not text", b"HTTP/1.1 200 OK\r\nX: \xff\r\n\r\n".to_vec()),
    ];
    for (expected, raw) in cases {
        let (port, _) = serve_once(vec![raw], false);
        let message = unusable(endpoint(port).status());
        assert!(message.contains(expected), "expected {expected:?}, got {message:?}");
    }
}

#[test]
fn oversized_headers_and_close_delimited_bodies_are_unusable() {
    let mut headers = b"HTTP/1.1 200 OK\r\n".to_vec();
    headers.extend(std::iter::repeat_n(b"X-Pad: 0123456789abcdef\r\n".as_slice(), 1000).flatten());
    let (port, _) = serve_once(vec![headers], false);
    assert!(unusable(endpoint(port).status()).contains("headers too large"));

    let mut raw = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    raw.resize(raw.len() + 1024 * 1024 + 10, b' ');
    let (port, _) = serve_once(vec![raw], true);
    assert!(unusable(endpoint(port).status()).contains("body too large"));
}

#[test]
fn an_unreachable_solar_agent_is_absent_but_a_silent_listener_is_not() {
    let message = absent(endpoint(unused_port()).status());
    assert!(message.contains("not running"), "{message}");

    let (port, _) = serve_once(vec![], true);
    let started = Instant::now();
    let result = endpoint(port).status();
    let mut chooser = SourceChooser::new(&result);
    assert_eq!(chooser.source(), Source::Agent, "a listening agent may still own the meter");
    assert_eq!(chooser.update(&result, ABSENT_GRACE_MS + 1), Source::Agent);
    let message = unusable(result);
    assert!(message.contains("did not answer"), "{message}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn incomplete_agent_status_never_triggers_direct_fallback() {
    for body in [
        edit_status(|s| s["companion"]["mode"] = "unknown-mode".into()),
        edit_status(|s| s["companion"]["snapshot"] = serde_json::Value::Null),
        edit_status(|s| {
            s["companion"].as_object_mut().unwrap().remove("snapshot");
        }),
        edit_status(|s| {
            s["companion"].as_object_mut().unwrap().remove("mode");
        }),
    ] {
        let result = parse_status(&body);
        let mut chooser = SourceChooser::new(&result);
        assert_eq!(chooser.source(), Source::Agent);
        assert_eq!(chooser.update(&result, ABSENT_GRACE_MS + 1), Source::Agent);
        unusable(result);
    }
}

#[test]
fn chooser_keeps_off_the_meter_while_solar_agent_answers() {
    let ok: Result<(), AgentError> = Ok(());
    let gone: Result<(), AgentError> = Err(AgentError::Absent("gone".into()));
    let broken: Result<(), AgentError> = Err(AgentError::Unusable("broken".into()));

    assert_eq!(SourceChooser::new(&gone).source(), Source::Direct);
    assert_eq!(SourceChooser::new(&broken).source(), Source::Agent);

    let mut chooser = SourceChooser::new(&ok);
    assert_eq!(chooser.source(), Source::Agent);
    assert_eq!(chooser.update(&gone, 1_000), Source::Agent);
    assert_eq!(chooser.update(&gone, 1_000 + ABSENT_GRACE_MS - 1), Source::Agent);
    assert_eq!(chooser.update(&ok, 1_000 + ABSENT_GRACE_MS), Source::Agent, "an answer resets the grace");
    assert_eq!(chooser.update(&gone, 20_000), Source::Agent);
    assert_eq!(chooser.update(&broken, 25_000), Source::Agent, "an unusable answer still means it is running");
    assert_eq!(chooser.update(&gone, 30_000), Source::Agent);
    assert_eq!(chooser.update(&gone, 30_000 + ABSENT_GRACE_MS), Source::Direct);

    assert_eq!(chooser.update(&gone, 100_000), Source::Direct);
    assert_eq!(chooser.update(&broken, 101_000), Source::Agent, "Solar Agent came back");
    assert_eq!(chooser.update(&gone, 102_000), Source::Agent, "a fresh grace period starts");
}

/// A fake Solar Agent that answers every connection with `body`, counting requests.
fn fake_agent(body: Vec<u8>) -> (u16, std::sync::Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let served = count.clone();
    thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { return };
            read_request(&mut socket);
            served.fetch_add(1, Ordering::Relaxed);
            let _ = socket.write_all(&response("200 OK", "Transfer-Encoding: chunked\r\n", &chunked(&body, 512)));
        }
    });
    (port, count)
}

#[test]
fn link_reads_solar_agent_before_returning_and_applies_controls() {
    let (port, requests) = fake_agent(fixture("agent-status-simulated.json").into_bytes());
    let dir = temp_dir();
    let info = dir.join("settings.json");
    std::fs::write(&info, format!(r#"{{"url":"http://127.0.0.1:{port}/#token={TOKEN}","port":{port}}}"#)).unwrap();

    let link = AgentLink::start(Some(info.clone()));
    let data = link.latest().expect("first answer is read synchronously");
    assert!(data.snapshot.simulated);
    let before = requests.load(Ordering::Relaxed);
    assert!(before >= 1);
    link.control(&Control::Watts(-2800)).unwrap();
    assert!(requests.load(Ordering::Relaxed) > before);
    assert!(link.latest().is_ok());

    std::fs::remove_file(&info).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while link.latest().is_ok() {
        assert!(Instant::now() < deadline, "the link never noticed Solar Agent leaving");
        thread::sleep(Duration::from_millis(50));
    }
    assert!(absent(link.latest()).contains("has not started"));
    assert!(absent(link.control(&Control::Scenario("good".into()))).contains("has not started"));
    drop(link);
    std::fs::remove_dir_all(dir).unwrap();

    assert!(absent(AgentLink::start(None).latest()).contains("settings folder"));
}

fn round_trip(snapshot: &Snapshot) {
    let json = serde_json::to_string(snapshot).unwrap();
    let back: Snapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, snapshot, "{json}");
}

#[test]
fn snapshots_survive_a_json_round_trip() {
    round_trip(&parse_status(fixture("agent-status.json").as_bytes()).unwrap().snapshot);
    round_trip(&parse_status(fixture("agent-status-simulated.json").as_bytes()).unwrap().snapshot);
    let service = SolarService::start(SolarConfig::default());
    let deadline = Instant::now() + Duration::from_secs(10);
    while service.snapshot().evaluation.surplus_w.is_none() {
        assert!(Instant::now() < deadline, "no simulated reading");
        thread::sleep(Duration::from_millis(50));
    }
    let snapshot = service.snapshot();
    assert!(!snapshot.presets.is_empty());
    round_trip(&snapshot);
}
