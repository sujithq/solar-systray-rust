//! Opt-in desktop test of the actual executable; all network traffic stays on loopback.

use serde_json::json;
use solar_core::agent::ABSENT_GRACE_MS;
use solar_core::dsmr::synthetic_telegram;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::{NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetMenuItemCount, GetMenuStringW, GetSubMenu, GetWindowThreadProcessId, HMENU, MF_BYPOSITION,
    PostMessageW, SendMessageW, WM_APP, WM_CANCELMODE, WM_CLOSE, WM_CONTEXTMENU,
};
use windows::core::{PCWSTR, w};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";
const STATUS: &str = include_str!(r"..\..\solar-core\tests\fixtures\agent-status.json");

fn wait_for(what: &str, seconds: u64, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("solar-tray-smoke-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove isolated smoke-test settings");
    }
}

struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn start(mut serve: impl FnMut(&mut TcpStream, &AtomicBool) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
                        stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                        serve(&mut stream, &stopping);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("test server accept: {error}"),
                }
            }
        });
        Self { address, stop, worker: Some(worker) }
    }

    fn agent(response: Arc<Mutex<(u16, String)>>) -> Self {
        Self::start(move |stream, _| {
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0 && request.len() + n <= 8192, "invalid test HTTP request");
                request.extend_from_slice(&buffer[..n]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /api/status HTTP/1.1\r\n"), "{request}");
            assert!(request.contains(&format!("x-companion-token: {TOKEN}\r\n")));
            let (code, body) = &*response.lock().unwrap();
            write!(stream, "HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                .unwrap();
        })
    }

    fn write_info(&self, path: &std::path::Path) {
        let port = self.address.port();
        std::fs::write(
            path,
            json!({"port": port, "url": format!("http://127.0.0.1:{port}/#token={TOKEN}")}).to_string(),
        )
        .unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().expect("test server stopped cleanly");
    }
}

struct Tray {
    child: Child,
}

impl Tray {
    fn window(&self) -> Option<HWND> {
        let hwnd = unsafe { FindWindowW(w!("SolarSystrayWindow"), PCWSTR::null()) }.ok()?;
        self.owns(hwnd).then_some(hwnd)
    }

    fn owns(&self, hwnd: HWND) -> bool {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        pid == self.child.id()
    }

    fn icon(&self) -> Option<NOTIFYICONIDENTIFIER> {
        Some(NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: self.window()?,
            uID: 1,
            ..Default::default()
        })
    }

    fn popup(&self) -> Option<HWND> {
        let hwnd = unsafe { FindWindowW(w!("#32768"), PCWSTR::null()) }.ok()?;
        self.owns(hwnd).then_some(hwnd)
    }

    fn open_menu(&self) {
        let hwnd = self.window().expect("tray window exists");
        unsafe {
            PostMessageW(Some(hwnd), WM_APP + 1, WPARAM((700 << 16) | 300), LPARAM(WM_CONTEXTMENU as isize)).unwrap();
        }
        wait_for("the tray's native popup menu", 5, || self.popup().is_some());
    }

    fn open_menu_text(&self) -> String {
        assert!(self.popup().is_some(), "the native menu remains open");
        // MN_GETHMENU obtains the menu displayed by the system popup window.
        let handle = unsafe { SendMessageW(self.popup().unwrap(), 0x01E1, None, None) };
        let mut lines = Vec::new();
        menu_lines(HMENU(handle.0 as *mut _), &mut lines);
        lines.join("\n")
    }

    fn close_menu(&self) {
        let hwnd = self.window().expect("tray window exists");
        unsafe { PostMessageW(Some(hwnd), WM_CANCELMODE, WPARAM(0), LPARAM(0)) }.unwrap();
        wait_for("menu dismissal", 5, || self.popup().is_none());
    }

    fn menu(&self) -> String {
        self.open_menu();
        let text = self.open_menu_text();
        self.close_menu();
        text
    }

    fn quit(&mut self) {
        let hwnd = self.window().expect("tray window exists");
        unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.unwrap();
        wait_for("graceful tray exit", 5, || self.child.try_wait().unwrap().is_some());
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().expect("stop only the test's own tray process");
        }
        self.child.wait().unwrap();
    }
}

fn menu_lines(menu: HMENU, lines: &mut Vec<String>) {
    assert!(!menu.0.is_null(), "native menu handle");
    let count = unsafe { GetMenuItemCount(Some(menu)) };
    assert!(count >= 0);
    for index in 0..count {
        let mut text = [0u16; 512];
        let length = unsafe { GetMenuStringW(menu, index as u32, Some(&mut text), MF_BYPOSITION) };
        lines.push(String::from_utf16_lossy(&text[..length as usize]));
        let sub = unsafe { GetSubMenu(menu, index) };
        if !sub.0.is_null() {
            menu_lines(sub, lines);
        }
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop with Explorer and no other Solar Systray instance"]
fn native_tray_switches_sources_without_competing_for_the_meter() {
    assert!(
        unsafe { FindWindowW(w!("SolarSystrayWindow"), PCWSTR::null()) }.is_err(),
        "quit your existing Solar Systray before running this opt-in test"
    );
    let dir = TestDirectory::new();
    let connections = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let meter = {
        let (connections, active) = (connections.clone(), active.clone());
        Server::start(move |stream, stop| {
            connections.fetch_add(1, Ordering::SeqCst);
            active.fetch_add(1, Ordering::SeqCst);
            let telegram = synthetic_telegram(0.0, 2450.0, "");
            while !stop.load(Ordering::SeqCst) && stream.write_all(telegram.as_bytes()).is_ok() {
                thread::sleep(Duration::from_millis(100));
            }
            active.fetch_sub(1, Ordering::SeqCst);
        })
    };
    let polls = Arc::new(AtomicUsize::new(0));
    let inverter = {
        let polls = polls.clone();
        Server::start(move |stream, _| {
            let mut request = [0; 12];
            stream.read_exact(&mut request).unwrap();
            polls.fetch_add(1, Ordering::SeqCst);
            let mut frame = [0; 41];
            frame[..2].copy_from_slice(&request[..2]);
            frame[4..6].copy_from_slice(&35u16.to_be_bytes());
            frame[6] = request[6];
            frame[7] = 4;
            frame[8] = 32;
            frame[9..11].copy_from_slice(&3000u16.to_be_bytes());
            frame[37..39].copy_from_slice(&2900u16.to_be_bytes());
            stream.write_all(&frame).unwrap();
        })
    };
    let response = Arc::new(Mutex::new((200, STATUS.to_string())));
    let agent = Server::agent(response.clone());
    let info = dir.0.join("agent-info.json");
    agent.write_info(&info);
    let config_dir = dir.0.join("Solar Systray");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        json!({
            "solar": {
                "energySource": {"type": "slimmelezer", "host": "127.0.0.1", "port": meter.address.port()},
                "inverterSource": {"host": "127.0.0.1", "port": inverter.address.port()}
            },
            "tray": {"notifications": {"enabled": false}}
        })
        .to_string(),
    )
    .unwrap();
    let mut tray = Tray {
        child: Command::new(env!("CARGO_BIN_EXE_solar-systray"))
            .env("LOCALAPPDATA", &dir.0)
            .env("SOLAR_AGENT_SETTINGS_INFO", &info)
            .spawn()
            .unwrap(),
    };
    wait_for("an icon registered with Explorer", 10, || {
        tray.icon().is_some_and(|id| unsafe { Shell_NotifyIconGetRect(&id) }.is_ok())
    });
    let menu = tray.menu();
    assert!(menu.contains("Read by\tSolar Agent") && menu.contains("1250 W"), "{menu}");
    assert_eq!(connections.load(Ordering::SeqCst), 0);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    eprintln!("PASS: native tray icon and agent values; no meter or inverter connection");

    tray.open_menu();
    assert!(tray.open_menu_text().contains("1250 W"));
    let mut updated_status: serde_json::Value = serde_json::from_str(STATUS).unwrap();
    updated_status["companion"]["snapshot"]["gridImportW"] = 4321.into();
    *response.lock().unwrap() = (200, updated_status.to_string());
    let mut updated_menu = String::new();
    wait_for("the open menu to show the newest status", 6, || {
        updated_menu = tray.open_menu_text();
        updated_menu.contains("4321 W")
    });
    assert!(tray.popup().is_some(), "the menu updated without closing or reopening");
    tray.close_menu();
    eprintln!("PASS: an already-open native menu refreshes when Solar Agent publishes new data");

    *response.lock().unwrap() = (401, "{}".into());
    thread::sleep(Duration::from_secs(4));
    let menu = tray.menu();
    assert!(menu.contains("NO DATA FROM SOLAR AGENT") && menu.contains("token"), "{menu}");
    thread::sleep(Duration::from_millis(ABSENT_GRACE_MS as u64 + 1000));
    assert_eq!(connections.load(Ordering::SeqCst), 0);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    eprintln!("PASS: unusable agent status stays off both devices beyond the fallback grace");

    drop(agent);
    thread::sleep(Duration::from_secs(4));
    assert_eq!(connections.load(Ordering::SeqCst), 0, "no connection during the grace period");
    wait_for("direct meter fallback", 16, || active.load(Ordering::SeqCst) == 1 && polls.load(Ordering::SeqCst) > 0);
    thread::sleep(Duration::from_secs(2));
    let menu = tray.menu();
    assert!(menu.contains("Read by\tSolar Systray") && menu.contains("2450 W") && menu.contains("3.0 kW"), "{menu}");
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    eprintln!("PASS: after the agent stops, direct DSMR and Sungrow readings reach the native menu");

    *response.lock().unwrap() = (200, STATUS.to_string());
    let agent = Server::agent(response);
    agent.write_info(&info);
    wait_for("meter release after agent restart", 8, || active.load(Ordering::SeqCst) == 0);
    let menu = tray.menu();
    assert!(menu.contains("Read by\tSolar Agent") && menu.contains("1250 W"), "{menu}");
    let inverter_polls = polls.load(Ordering::SeqCst);
    thread::sleep(Duration::from_secs(3));
    assert_eq!(connections.load(Ordering::SeqCst), 1, "no direct reconnect in agent mode");
    assert_eq!(polls.load(Ordering::SeqCst), inverter_polls, "no inverter polling in agent mode");
    eprintln!("PASS: restarting the agent closes the direct meter connection and stops inverter polling");

    let icon = tray.icon().unwrap();
    tray.quit();
    assert!(unsafe { Shell_NotifyIconGetRect(&icon) }.is_err(), "quit removes the shell icon");
    eprintln!("PASS: graceful quit removes the tray icon");
}
