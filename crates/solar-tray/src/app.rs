//! The Win32 side: a hidden window that owns the notification-area icon, its menu and balloons.
//! Solar data comes from a running Solar Agent ([`AgentLink`]) or, when there is none, from the
//! tray's own [`SolarService`]; a one-second timer refreshes what is shown.

use crate::alerts::{Alert, Alerts, Kind};
use crate::icon::{self, IconSpec};
use crate::settings::{self, IconStyle, Seed, TraySettings};
use crate::{autostart, view};
use serde_json::Value;
use solar_core::agent::{self, AgentLink, AgentResult, Control, Source, SourceChooser};
use solar_core::mock::{MOCK_SCENARIOS, scenario_label};
use solar_core::present::{self, Row};
use solar_core::{Snapshot, SolarConfig, SolarService};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONUSERCLICK, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CheckMenuRadioItem, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMenuItemCount, GetMenuStringW, GetMessageW,
    GetSubMenu, GetSystemMetrics, HICON, HMENU, ICONINFO, KillTimer, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    MENU_ITEM_FLAGS, MENUITEMINFOW, MF_BYCOMMAND, MF_BYPOSITION, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, MIIM_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
    SM_CXSMICON, SM_MENUDROPALIGNMENT, SW_SHOWNORMAL, SetForegroundWindow, SetMenuDefaultItem, SetMenuItemInfoW,
    SetTimer, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON,
    TrackPopupMenuEx, TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_ENDSESSION, WM_NULL, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, PWSTR, w};

const WM_TRAY: u32 = WM_APP + 1;
/// `NIN_SELECT | NINF_KEY`: the icon was activated with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const TIMER: usize = 1;
const TRAY_ID: u32 = 1;
const CLASS: PCWSTR = w!("SolarSystrayWindow");
const TITLE: PCWSTR = w!("Solar Systray");
/// Notes waiting for the icon (for example while Explorer starts) are capped.
const MAX_NOTES: usize = 4;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app unless it is already borrowed (a nested message loop re-entered).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut app| app.as_mut().map(f)))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `text` into a fixed, NUL-terminated buffer, truncating on a character boundary.
fn copy_into(target: &mut [u16], text: &str) {
    let mut length = 0;
    for c in text.chars() {
        let mut units = [0u16; 2];
        let units = c.encode_utf16(&mut units);
        if length + units.len() >= target.len() {
            break;
        }
        target[length..length + units.len()].copy_from_slice(units);
        length += units.len();
    }
    target[length] = 0;
}

pub fn message_box(text: &str, error: bool) {
    let text = wide(text);
    let style = if error { MB_OK | MB_ICONERROR } else { MB_OK | MB_ICONINFORMATION };
    let _ = unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), TITLE, style) };
}

#[derive(Debug, Clone)]
struct Note {
    title: String,
    body: String,
    warning: bool,
}

impl Note {
    fn new(title: impl Into<String>, body: impl Into<String>, warning: bool) -> Self {
        Note { title: title.into(), body: body.into(), warning }
    }
}

impl From<Alert> for Note {
    fn from(alert: Alert) -> Self {
        Note { title: alert.title, body: alert.body, warning: alert.warning }
    }
}

#[derive(Debug, Clone)]
enum Action {
    None,
    Quit,
    OpenConfig,
    ReloadConfig,
    ToggleAutostart,
    Notify(Option<Kind>),
    Style(IconStyle),
    Appliance(String),
    Scenario(&'static str),
    Watts(i64),
}

/// Where the shown data comes from.
enum Feed {
    /// The tray's own meter and inverter connections.
    Direct(SolarService),
    /// Solar Agent's newest answer; the tray opens no device connections.
    Agent(AgentResult),
}

struct Running {
    feed: Feed,
    solar_json: Value,
    config: SolarConfig,
    tray: TraySettings,
    alerts: Alerts,
}

impl Running {
    /// What to show, or why Solar Agent's data cannot be shown.
    fn snapshot(&self) -> Result<Snapshot, String> {
        match &self.feed {
            Feed::Direct(service) => Ok(service.snapshot()),
            Feed::Agent(Ok(data)) => Ok(data.snapshot.clone()),
            Feed::Agent(Err(error)) => Err(error.to_string()),
        }
    }

    fn service(&self) -> Option<&SolarService> {
        match &self.feed {
            Feed::Direct(service) => Some(service),
            Feed::Agent(_) => None,
        }
    }
}

enum Mode {
    Running(Box<Running>),
    /// The config file cannot be used; the message explains why.
    Broken(String),
}

fn source_text(running: &Running) -> String {
    match &running.feed {
        Feed::Agent(_) => "your Solar Agent's data".into(),
        Feed::Direct(_) if running.config.is_mock() => "a simulation".into(),
        Feed::Direct(_) => {
            let source = &running.config.energy_source;
            format!("the SlimmeLezer at {}:{}", source.host, source.port)
        }
    }
}

fn start_direct(config: &SolarConfig, tray: &TraySettings) -> SolarService {
    let service = SolarService::start(config.clone());
    if let Some(id) = &tray.appliance {
        // An appliance that no longer exists falls back to the configured default.
        let _ = service.select_appliance(id);
    }
    service
}

/// Loads the config. With `agent` (Solar Agent's answer) the tray shows Solar Agent's data and
/// opens no connections; otherwise it connects to the meter and inverter itself.
fn load(path: &Path, companion: Option<&Path>, agent: Option<AgentResult>) -> (Mode, Option<Note>) {
    let loaded = match settings::load_or_seed(path, companion) {
        Ok(loaded) => loaded,
        Err(error) => {
            return (Mode::Broken(error.clone()), Some(Note::new("Solar Systray: config error", error, true)));
        }
    };
    let feed = match agent {
        Some(latest) => Feed::Agent(latest),
        None => Feed::Direct(start_direct(&loaded.solar, &loaded.tray)),
    };
    let running = Running {
        feed,
        solar_json: loaded.solar_json,
        config: loaded.solar,
        tray: loaded.tray,
        alerts: Alerts::default(),
    };
    let note = match (&loaded.seeded, &running.feed) {
        (None, _) => None,
        (Some(_), Feed::Agent(_)) => Some(Note::new(
            "Solar Systray is running",
            "Showing your Solar Agent's data. Click the icon for details.",
            false,
        )),
        (Some(Seed::Companion(_)), Feed::Direct(_)) => Some(Note::new(
            "Solar Systray is running",
            format!("Using your Solar Agent settings: {}. Click the icon for details.", source_text(&running)),
            false,
        )),
        (Some(Seed::Defaults), Feed::Direct(_)) => Some(Note::new(
            "Solar Systray is running a simulation",
            "No Solar Agent settings were found. Choose \"Open config file\" in the menu to set your SlimmeLezer address.",
            false,
        )),
    };
    (Mode::Running(Box::new(running)), note)
}

fn icon_size(hwnd: HWND) -> u32 {
    let dpi = match unsafe { GetDpiForWindow(hwnd) } {
        0 => unsafe { GetDpiForSystem() },
        dpi => dpi,
    };
    match unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) } {
        size if size > 0 => (size as u32).min(256),
        _ => 16,
    }
}

fn create_icon(spec: &IconSpec, size: u32) -> windows::core::Result<HICON> {
    let pixels = icon::render(spec, size);
    let n = size as i32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: n,
            biHeight: -n,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    unsafe {
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let target = std::slice::from_raw_parts_mut(bits.cast::<[u8; 4]>(), pixels.len());
        for (pixel, [r, g, b, a]) in target.iter_mut().zip(pixels) {
            *pixel = [b, g, r, a];
        }
        // The alpha channel decides transparency; the monochrome mask only has to exist.
        let mask_bits = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
        let mask = CreateBitmap(n, n, 1, 1, Some(mask_bits.as_ptr().cast()));
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        });
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

fn open_file(path: &Path) {
    let file = wide(&path.to_string_lossy());
    let opened = unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
    };
    if opened.0 as isize <= 32 {
        let quoted = wide(&format!("\"{}\"", path.display()));
        unsafe {
            ShellExecuteW(None, w!("open"), w!("notepad.exe"), PCWSTR(quoted.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL)
        };
    }
}

/// Collects menu items; command ids are indexes into `actions` plus one.
struct MenuBuilder {
    actions: Vec<Action>,
}

struct ActiveMenu {
    root: HMENU,
    actions: Vec<Action>,
}

const MENU_TEXT_CAPACITY: usize = 2048;

fn same_menu_shape(current: HMENU, updated: HMENU) -> bool {
    let current_count = unsafe { GetMenuItemCount(Some(current)) };
    if current_count < 0 || current_count != unsafe { GetMenuItemCount(Some(updated)) } {
        return false;
    }
    (0..current_count).all(|index| {
        let current_sub = unsafe { GetSubMenu(current, index) };
        let updated_sub = unsafe { GetSubMenu(updated, index) };
        match (!current_sub.0.is_null(), !updated_sub.0.is_null()) {
            (true, true) => same_menu_shape(current_sub, updated_sub),
            (false, false) => true,
            _ => false,
        }
    })
}

/// Updates the labels of an already-visible menu without replacing any active submenu handles.
fn update_menu_labels(current: HMENU, updated: HMENU) -> bool {
    if !same_menu_shape(current, updated) {
        return false;
    }
    let count = unsafe { GetMenuItemCount(Some(current)) };
    for index in 0..count {
        let current_sub = unsafe { GetSubMenu(current, index) };
        let updated_sub = unsafe { GetSubMenu(updated, index) };
        if !current_sub.0.is_null() {
            if !update_menu_labels(current_sub, updated_sub) {
                return false;
            }
            continue;
        }

        let mut current_text = [0u16; MENU_TEXT_CAPACITY];
        let current_length = unsafe { GetMenuStringW(current, index as u32, Some(&mut current_text), MF_BYPOSITION) };
        let mut updated_text = [0u16; MENU_TEXT_CAPACITY];
        let updated_length = unsafe { GetMenuStringW(updated, index as u32, Some(&mut updated_text), MF_BYPOSITION) };
        if updated_length == 0 {
            continue;
        }
        if current_length == updated_length
            && current_text[..current_length as usize] == updated_text[..updated_length as usize]
        {
            continue;
        }
        let item = MENUITEMINFOW {
            cbSize: size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_STRING,
            dwTypeData: PWSTR(updated_text.as_mut_ptr()),
            cch: updated_length as u32,
            ..Default::default()
        };
        if unsafe { SetMenuItemInfoW(current, index as u32, true, &item) }.is_err() {
            return false;
        }
    }
    true
}

impl MenuBuilder {
    fn popup() -> HMENU {
        unsafe { CreatePopupMenu() }.unwrap_or_default()
    }

    fn add(&mut self, menu: HMENU, text: &str, action: Action, flags: MENU_ITEM_FLAGS) -> u32 {
        self.actions.push(action);
        let id = self.actions.len();
        let text = wide(&text.replace('&', "&&"));
        let _ = unsafe { AppendMenuW(menu, MF_STRING | flags, id, PCWSTR(text.as_ptr())) };
        id as u32
    }

    /// A read-only line; it stays enabled so it is easy to read.
    fn info(&mut self, menu: HMENU, text: &str) -> u32 {
        self.add(menu, text, Action::None, MF_STRING)
    }

    fn row(&mut self, menu: HMENU, row: &Row) -> u32 {
        self.info(menu, &format!("{}\t{}", row.label, row.value))
    }

    fn check(&mut self, menu: HMENU, text: &str, action: Action, checked: bool) -> u32 {
        self.add(menu, text, action, if checked { MF_CHECKED } else { MF_STRING })
    }

    fn separator(menu: HMENU) {
        let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };
    }

    fn submenu(menu: HMENU, text: &str, sub: HMENU) {
        let text = wide(&text.replace('&', "&&"));
        let _ = unsafe { AppendMenuW(menu, MF_POPUP, sub.0 as usize, PCWSTR(text.as_ptr())) };
    }

    /// Adds a radio group and marks `selected`.
    fn radio<T: PartialEq>(&mut self, menu: HMENU, items: Vec<(String, Action, T)>, selected: &T) {
        let mut range: Option<(u32, u32)> = None;
        let mut checked = None;
        for (text, action, value) in items {
            let id = self.add(menu, &text, action, MF_STRING);
            range = Some((range.map_or(id, |r| r.0), id));
            if value == *selected {
                checked = Some(id);
            }
        }
        if let (Some((first, last)), Some(checked)) = (range, checked) {
            let _ = unsafe { CheckMenuRadioItem(menu, first, last, checked, MF_BYCOMMAND.0) };
        }
    }
}

struct App {
    hwnd: HWND,
    path: PathBuf,
    companion: Option<PathBuf>,
    mode: Mode,
    agent: AgentLink,
    chooser: SourceChooser,
    started: Instant,
    added: bool,
    icon: Option<(IconSpec, u32, HICON)>,
    tip: String,
    notes: VecDeque<Note>,
    active_menu: Option<ActiveMenu>,
}

impl App {
    fn note(&mut self, note: Note) {
        if self.notes.len() >= MAX_NOTES {
            self.notes.pop_front();
        }
        self.notes.push_back(note);
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY,
            ..Default::default()
        };
        if let Some((_, _, icon)) = &self.icon {
            data.hIcon = *icon;
            data.uFlags |= NIF_ICON;
        }
        copy_into(&mut data.szTip, &self.tip);
        data
    }

    /// Takes Solar Agent's newest answer, and switches between its data and the tray's own
    /// connections when Solar Agent starts or has been gone for a while.
    fn follow_agent(&mut self) {
        let latest = self.agent.latest();
        let source = self.chooser.update(&latest, self.started.elapsed().as_millis() as i64);
        let Mode::Running(running) = &mut self.mode else { return };
        match (&mut running.feed, source) {
            (Feed::Agent(shown), Source::Agent) => *shown = latest,
            (Feed::Direct(_), Source::Direct) => {}
            (Feed::Direct(_), Source::Agent) => {
                // Dropping the service closes the tray's meter connection, so Solar Agent gets the meter.
                running.feed = Feed::Agent(latest);
                running.alerts = Alerts::default();
            }
            (Feed::Agent(_), Source::Direct) => {
                running.feed = Feed::Direct(start_direct(&running.config, &running.tray));
                running.alerts = Alerts::default();
            }
        }
    }

    fn tick(&mut self) {
        self.follow_agent();
        let (spec, tip, alert) = match &mut self.mode {
            Mode::Running(running) => match running.snapshot() {
                Ok(snapshot) => {
                    let tray = &running.tray;
                    let alert = running
                        .alerts
                        .update(&snapshot, &tray.notifications, tray.offline_notify_after_seconds)
                        .into_iter()
                        .next();
                    (view::icon_spec(&snapshot, tray.icon_style), present::tooltip(&snapshot), alert)
                }
                Err(problem) => (view::error_spec(), present::fit_tooltip(vec![problem]), None),
            },
            Mode::Broken(error) => {
                let reason = error.strip_prefix(&format!("{}: ", self.path.display())).unwrap_or(error);
                (view::error_spec(), present::fit_tooltip(vec![format!("Config error: {reason}")]), None)
            }
        };
        if let Some(alert) = alert {
            self.note(alert.into());
        }
        self.show(spec, tip);
        self.refresh_active_menu();
        if self.added
            && let Some(note) = self.notes.pop_front()
        {
            self.balloon(&note);
        }
    }

    /// Adds the icon (retrying until Explorer accepts it) or updates it when something changed.
    fn show(&mut self, spec: IconSpec, tip: String) {
        let size = icon_size(self.hwnd);
        let mut retired = None;
        let mut changed = false;
        if !matches!(&self.icon, Some((shown, n, _)) if *shown == spec && *n == size)
            && let Ok(icon) = create_icon(&spec, size)
        {
            retired = self.icon.replace((spec, size, icon)).map(|(_, _, old)| old);
            changed = true;
        }
        if self.tip != tip {
            self.tip = tip;
            changed = true;
        }
        if !self.added {
            let mut data = self.data();
            if unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
                self.added = true;
            } else {
                // A stale icon with our id blocks NIM_ADD; clear it for the next attempt.
                let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
            }
        } else if changed && !unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data()) }.as_bool() {
            self.added = false;
        }
        if let Some(old) = retired {
            let _ = unsafe { DestroyIcon(old) };
        }
    }

    fn balloon(&self, note: &Note) {
        let mut data = self.data();
        data.uFlags |= NIF_INFO;
        copy_into(&mut data.szInfoTitle, &note.title);
        copy_into(&mut data.szInfo, &note.body);
        data.dwInfoFlags = (if note.warning { NIIF_WARNING } else { NIIF_INFO }) | NIIF_RESPECT_QUIET_TIME;
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }

    fn remove_icon(&mut self) {
        if self.added {
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data()) };
            self.added = false;
        }
    }

    fn build_menu(&self) -> Option<ActiveMenu> {
        let root = unsafe { CreatePopupMenu() }.ok()?;
        let mut menu = MenuBuilder { actions: Vec::new() };
        match &self.mode {
            Mode::Running(running) => solar_menu(&mut menu, root, running),
            Mode::Broken(error) => {
                menu.info(root, "CONFIG ERROR");
                let reason = error.strip_prefix(&format!("{}: ", self.path.display())).unwrap_or(error);
                let short: String = reason.chars().take(120).collect();
                menu.info(root, &short);
            }
        }
        MenuBuilder::separator(root);
        menu.check(root, "Start at sign-in", Action::ToggleAutostart, autostart::is_enabled());
        menu.add(root, "Open config file", Action::OpenConfig, MF_STRING);
        menu.add(root, "Reload config", Action::ReloadConfig, MF_STRING);
        MenuBuilder::separator(root);
        menu.add(root, "Quit", Action::Quit, MF_STRING);
        let _ = unsafe { SetMenuDefaultItem(root, 0, 1) };
        Some(ActiveMenu { root, actions: menu.actions })
    }

    fn open_menu(&mut self) -> Option<HMENU> {
        if self.active_menu.is_some() {
            return None;
        }
        self.active_menu = self.build_menu();
        self.active_menu.as_ref().map(|menu| menu.root)
    }

    fn refresh_active_menu(&mut self) {
        let Some(active) = &self.active_menu else { return };
        let Some(updated) = self.build_menu() else { return };
        if update_menu_labels(active.root, updated.root)
            && let Some(active) = &mut self.active_menu
        {
            active.actions = updated.actions;
        }
        let _ = unsafe { DestroyMenu(updated.root) };
    }

    fn close_menu(&mut self, command: usize) -> Option<Action> {
        let active = self.active_menu.take()?;
        let action = command.checked_sub(1).and_then(|index| active.actions.into_iter().nth(index));
        let _ = unsafe { DestroyMenu(active.root) };
        action
    }

    fn perform(&mut self, action: Action) {
        let running = match &self.mode {
            Mode::Running(running) => Some(running.as_ref()),
            Mode::Broken(_) => None,
        };
        // Solar Agent applies controls itself, so the tray does not save them as preferences.
        let via_agent = matches!(running, Some(Running { feed: Feed::Agent(_), .. }));
        let service = running.and_then(Running::service);
        let control = |result: Result<(), solar_core::runtime::ControlError>| result.err().map(|e| e.to_string());
        let failure = match action {
            Action::None => None,
            Action::Quit => {
                let _ = unsafe { PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
                return;
            }
            Action::OpenConfig => {
                open_file(&self.path);
                None
            }
            Action::ReloadConfig => {
                self.reload();
                None
            }
            Action::ToggleAutostart => autostart::set_enabled(!autostart::is_enabled()).err(),
            Action::Notify(kind) => self.change_tray(|tray| {
                let flag = tray.notifications.flag_mut(kind);
                *flag = !*flag;
            }),
            Action::Style(style) => self.change_tray(|tray| tray.icon_style = style),
            Action::Appliance(id) if via_agent => self.tell_agent(Control::Scenario(id)),
            Action::Scenario(name) if via_agent => self.tell_agent(Control::Scenario(name.to_string())),
            Action::Watts(watts) if via_agent => self.tell_agent(Control::Watts(watts)),
            Action::Appliance(id) => match service.map(|s| s.select_appliance(&id)) {
                Some(Ok(())) => self.change_tray(|tray| tray.appliance = Some(id)),
                Some(Err(error)) => Some(error.to_string()),
                None => None,
            },
            Action::Scenario(name) => service.and_then(|s| control(s.set_scenario(name))),
            Action::Watts(watts) => service.and_then(|s| control(s.set_simulated_watts(watts))),
        };
        if let Some(error) = failure {
            self.note(Note::new("Solar Systray", error, true));
        }
        self.tick();
    }

    /// Sends a control to Solar Agent; returns an error message when it is not applied.
    fn tell_agent(&self, control: Control) -> Option<String> {
        self.agent.control(&control).err().map(|error| error.to_string())
    }

    /// Applies a tray preference and saves it; returns an error message when saving fails.
    fn change_tray(&mut self, change: impl FnOnce(&mut TraySettings)) -> Option<String> {
        let Mode::Running(running) = &mut self.mode else { return None };
        change(&mut running.tray);
        settings::save_tray(&self.path, &running.solar_json, &running.tray)
            .err()
            .map(|e| format!("Cannot save {}: {e}", self.path.display()))
    }

    fn reload(&mut self) {
        // Stop the old connections before the new ones start.
        self.mode = Mode::Broken(String::new());
        let agent = (self.chooser.source() == Source::Agent).then(|| self.agent.latest());
        let (mode, note) = load(&self.path, self.companion.as_deref(), agent);
        let note = note.or_else(|| match &mode {
            Mode::Running(running) => {
                Some(Note::new("Config reloaded", format!("Showing {}.", source_text(running)), false))
            }
            Mode::Broken(_) => None,
        });
        self.mode = mode;
        if let Some(note) = note {
            self.note(note);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(menu) = self.active_menu.take() {
            let _ = unsafe { DestroyMenu(menu.root) };
        }
        self.remove_icon();
        if let Some((_, _, icon)) = self.icon.take() {
            let _ = unsafe { DestroyIcon(icon) };
        }
    }
}

fn solar_menu(menu: &mut MenuBuilder, root: HMENU, running: &Running) {
    match running.snapshot() {
        Ok(snapshot) => snapshot_menu(menu, root, running, &snapshot),
        Err(problem) => {
            menu.info(root, "NO DATA FROM SOLAR AGENT");
            let short: String = problem.chars().take(120).collect();
            menu.info(root, &short);
            MenuBuilder::separator(root);
        }
    }

    let style = MenuBuilder::popup();
    let styles = vec![
        ("Level symbol".to_string(), Action::Style(IconStyle::Level), IconStyle::Level),
        ("Power in kW".to_string(), Action::Style(IconStyle::Power), IconStyle::Power),
    ];
    menu.radio(style, styles, &running.tray.icon_style);
    MenuBuilder::submenu(root, "Icon style", style);

    let notifications = MenuBuilder::popup();
    let settings = &running.tray.notifications;
    menu.check(notifications, "Show notifications", Action::Notify(None), settings.enabled);
    MenuBuilder::separator(notifications);
    for kind in Kind::ALL {
        let mut flags = if settings.wants(kind) { MF_CHECKED } else { MF_STRING };
        if !settings.enabled {
            flags |= MF_GRAYED;
        }
        menu.add(notifications, kind.label(), Action::Notify(Some(kind)), flags);
    }
    MenuBuilder::submenu(root, "Notifications", notifications);
}

/// The solar part of the menu: status, appliance, details and (when simulated) the simulation.
fn snapshot_menu(menu: &mut MenuBuilder, root: HMENU, running: &Running, snapshot: &Snapshot) {
    let info = present::solar_info(snapshot);
    let sim = if snapshot.simulated { "   [SIMULATION]" } else { "" };
    menu.info(root, &format!("{}{sim}", info.headline.title));
    menu.info(root, &info.headline.reason);
    MenuBuilder::separator(root);
    for row in info.section("Power now").map_or(&[][..], |s| s.rows.as_slice()) {
        menu.row(root, row);
    }
    MenuBuilder::separator(root);

    let appliance = MenuBuilder::popup();
    if let Some(starts) = info.section("Appliance").and_then(|s| s.rows.get(1)) {
        menu.row(appliance, starts);
        MenuBuilder::separator(appliance);
    }
    let (appliances, selected) = match &running.feed {
        Feed::Direct(service) => (&running.config.appliances, service.appliance()),
        Feed::Agent(Ok(data)) => (&data.appliances, snapshot.evaluation.appliance.clone()),
        Feed::Agent(Err(_)) => (&running.config.appliances, snapshot.evaluation.appliance.clone()),
    };
    let items = appliances
        .iter()
        .map(|(id, a)| (format!("{}\t{}", a.name, present::watts(a.peak_w)), Action::Appliance(id.clone()), id.clone()))
        .collect();
    menu.radio(appliance, items, &selected);
    MenuBuilder::submenu(root, &format!("Appliance\t{}", snapshot.evaluation.appliance_name), appliance);

    for title in ["Quarter-hour peak", "Today", "Connections"] {
        if let Some(section) = info.section(title) {
            let sub = MenuBuilder::popup();
            if title == "Connections" {
                let reader = if matches!(running.feed, Feed::Agent(_)) { "Solar Agent" } else { "Solar Systray" };
                menu.info(sub, &format!("Read by\t{reader}"));
            }
            for row in &section.rows {
                menu.row(sub, row);
            }
            MenuBuilder::submenu(root, title, sub);
        }
    }
    MenuBuilder::separator(root);

    if snapshot.simulated {
        let simulation = MenuBuilder::popup();
        let scenarios = MOCK_SCENARIOS
            .iter()
            .map(|(name, _)| (scenario_label(name).to_string(), Action::Scenario(name), Some(name.to_string())))
            .collect();
        menu.radio(simulation, scenarios, &snapshot.scenario);
        MenuBuilder::separator(simulation);
        let power = MenuBuilder::popup();
        for preset in &snapshot.presets {
            let text = format!("{}\t{} W", preset.label, preset.watts);
            menu.check(power, &text, Action::Watts(preset.watts), snapshot.simulated_w == Some(preset.watts));
        }
        MenuBuilder::submenu(simulation, "Set grid power", power);
        MenuBuilder::submenu(root, "Simulation", simulation);
    }
}

fn show_menu(hwnd: HWND, x: i32, y: i32) {
    let Some(Some(menu)) = with_app(App::open_menu) else { return };
    let align = if unsafe { GetSystemMetrics(SM_MENUDROPALIGNMENT) } != 0 { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
    let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | align;
    let command = unsafe {
        // The window must be in the foreground or the menu will not close when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenuEx(menu, flags.0, x, y, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        command.0 as usize
    };
    if let Some(Some(action)) = with_app(|app| app.close_menu(command)) {
        with_app(|app| app.perform(action));
    }
}

fn cursor() -> (i32, i32) {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    (point.x, point.y)
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_TIMER if wparam.0 == TIMER => {
            with_app(App::tick);
            LRESULT(0)
        }
        WM_TRAY => {
            match (lparam.0 & 0xFFFF) as u32 {
                WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT => {
                    // Version 4 passes the anchor point in wParam as two signed 16-bit values.
                    let x = (wparam.0 & 0xFFFF) as u16 as i16 as i32;
                    let y = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                    show_menu(hwnd, x, y);
                }
                NIN_BALLOONUSERCLICK => {
                    let (x, y) = cursor();
                    show_menu(hwnd, x, y);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            with_app(App::tick);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                with_app(App::remove_icon);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER) };
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // Explorer restarted: the icon is gone and has to be added again.
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            with_app(|app| {
                app.added = false;
                app.tick();
            });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn create_window() -> windows::core::Result<HWND> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(PCWSTR::null())?.into();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        // A hidden top-level window rather than a message-only one, which would not receive the
        // TaskbarCreated broadcast.
        CreateWindowExW(WS_EX_TOOLWINDOW, CLASS, TITLE, WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(instance), None)
    }
}

/// Holds a named mutex for the life of the process; `false` when another copy owns it.
fn single_instance() -> bool {
    match unsafe { CreateMutexW(None, true, w!(r"Local\SolarSystray.SingleInstance")) } {
        Ok(_) => (unsafe { GetLastError() }) != ERROR_ALREADY_EXISTS,
        Err(_) => true,
    }
}

pub fn run() -> Result<(), String> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    if !single_instance() {
        message_box("Solar Systray is already running. Look for its icon in the notification area.", false);
        return Ok(());
    }
    let base = settings::local_app_data().ok_or("The LOCALAPPDATA folder is not set.")?;
    let path = settings::config_path(&base);
    let companion = Some(settings::companion_path(&base));
    let hwnd = create_window().map_err(|e| format!("Cannot create the tray window: {}", e.message()))?;
    TASKBAR_CREATED.store(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }, Ordering::Relaxed);

    // Ask Solar Agent first, so the tray never opens a second meter connection next to it.
    let info = agent::settings_info_path(std::env::var_os(agent::SETTINGS_INFO_ENV), Some(&base));
    let agent = AgentLink::start(info);
    let latest = agent.latest();
    let chooser = SourceChooser::new(&latest);
    let (mode, note) = load(&path, companion.as_deref(), (chooser.source() == Source::Agent).then_some(latest));
    let mut app = App {
        hwnd,
        path,
        companion,
        mode,
        agent,
        chooser,
        started: Instant::now(),
        added: false,
        icon: None,
        tip: String::new(),
        notes: VecDeque::new(),
        active_menu: None,
    };
    if let Some(note) = note {
        app.note(note);
    }
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with_app(App::tick);
    unsafe { SetTimer(Some(hwnd), TIMER, 1000, None) };

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // Dropping the app removes the icon and stops the background threads.
    APP.with(|cell| cell.borrow_mut().take());
    Ok(())
}
