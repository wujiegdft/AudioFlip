#![windows_subsystem = "windows"]

mod audio;
mod autostart;
mod config;

use std::cell::RefCell;
use std::collections::HashMap;

use audio::{Audio, Device, Notifier};
use config::{Config, Entry};
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::IMMNotificationClient;
use windows::Win32::System::Com::*;
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_DEVICES_CHANGED: u32 = WM_APP + 2;
const TIMER_REFRESH: usize = 1;
const TRAY_UID: u32 = 1;
const CLICK_DEBOUNCE_MS: u64 = 300;
// Tray callback events for NOTIFYICON_VERSION_4 (shellapi.h).
const NIN_SELECT_EVT: u32 = WM_USER;
const NIN_KEYSELECT_EVT: u32 = WM_USER + 1;

const ID_AUTOSTART: usize = 1;
const ID_OPEN_CONFIG: usize = 2;
const ID_EXIT: usize = 3;
const ID_DEVICE_BASE: usize = 100;

struct App {
    hwnd: HWND,
    audio: Audio,
    config: Config,
    notifier: IMMNotificationClient,
    icons: HashMap<String, HICON>,
    warn_icon: HICON,
    last_click: u64,
    /// Device id/name per menu item, indexed by `cmd - ID_DEVICE_BASE`.
    menu_items: Vec<Entry>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Runs `f` against the app state. Returns None when the state is already borrowed, which
/// happens when a message is dispatched re-entrantly (popup menu loop, STA COM call pumping).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn copy_wstr(dst: &mut [u16], s: &str) {
    let mut n = 0;
    let max = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16()).take(max) {
        *d = c;
        n += 1;
    }
    dst[n] = 0;
}

fn icon_size() -> i32 {
    unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }
}

fn load_icon(spec: &str) -> Option<HICON> {
    let (file, index) = spec.rsplit_once(',')?;
    let index: i32 = index.trim().parse().ok()?;
    let src = wide(file.trim());
    let mut buf = [0u16; 520];
    let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf)) };
    if n == 0 || n as usize > buf.len() {
        return None;
    }
    let size = icon_size() as u32;
    let mut icon = HICON::default();
    unsafe {
        SHDefExtractIconW(PCWSTR(buf.as_ptr()), index, 0, Some(&mut icon), None, size).ok().ok()?;
    }
    (!icon.is_invalid()).then_some(icon)
}

fn warning_icon() -> HICON {
    let mut info = SHSTOCKICONINFO { cbSize: size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
    unsafe {
        if SHGetStockIconInfo(SIID_WARNING, SHGSI_ICON | SHGSI_SMALLICON, &mut info).is_ok() {
            return info.hIcon;
        }
        LoadIconW(None, IDI_WARNING).unwrap_or_default()
    }
}

impl App {
    fn nid(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_UID,
            ..Default::default()
        }
    }

    fn add_tray(&mut self) {
        let mut nid = self.nid();
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.warn_icon;
        copy_wstr(&mut nid.szTip, "AudioFlip");
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        }
        self.refresh();
    }

    fn balloon(&self, title: &str, msg: &str, flags: NOTIFY_ICON_INFOTIP_FLAGS) {
        let mut nid = self.nid();
        nid.uFlags = NIF_INFO;
        copy_wstr(&mut nid.szInfoTitle, title);
        copy_wstr(&mut nid.szInfo, msg);
        nid.dwInfoFlags = flags;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn icon_for(&mut self, d: &Device) -> HICON {
        if let Some(&h) = self.icons.get(&d.icon_path) {
            return h;
        }
        let h = load_icon(&d.icon_path).unwrap_or(self.warn_icon);
        self.icons.insert(d.icon_path.clone(), h);
        h
    }

    /// Maps whitelist entries to currently active devices, in whitelist order. Falls back to
    /// name matching when an id went stale (e.g. driver reinstall) and persists the new id.
    fn candidates(&mut self, devices: &[Device]) -> Vec<Device> {
        let mut out = Vec::new();
        let mut dirty = false;
        for e in self.config.devices.iter_mut() {
            let found = devices.iter().find(|d| d.id == e.id).or_else(|| {
                let mut by_name = devices.iter().filter(|d| d.name == e.name);
                match (by_name.next(), by_name.next()) {
                    (Some(d), None) => Some(d),
                    _ => None,
                }
            });
            if let Some(d) = found {
                if e.id != d.id || e.name != d.name {
                    e.id = d.id.clone();
                    e.name = d.name.clone();
                    dirty = true;
                }
                if !out.iter().any(|o: &Device| o.id == d.id) {
                    out.push(d.clone());
                }
            }
        }
        if dirty {
            let _ = self.config.save();
        }
        out
    }

    fn refresh(&mut self) {
        let devices = self.audio.devices();
        let cands = self.candidates(&devices);
        let cur = self.audio.default_id();
        let cur_dev = cur.as_ref().and_then(|c| devices.iter().find(|d| &d.id == c)).cloned();
        let (icon, tip) = match &cur_dev {
            Some(d) if cands.iter().any(|c| c.id == d.id) => (self.icon_for(d), d.name.clone()),
            Some(d) => (self.icon_for(d), format!("{}（不在轮换列表）", d.name)),
            None => (self.warn_icon, "无可用输出设备".to_string()),
        };
        let mut nid = self.nid();
        nid.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.hIcon = icon;
        copy_wstr(&mut nid.szTip, &format!("AudioFlip · {tip}"));
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    /// Returns false when there is nothing to toggle and the menu should be shown instead.
    fn toggle(&mut self) -> bool {
        let devices = self.audio.devices();
        let cands = self.candidates(&devices);
        if cands.len() < 2 {
            return false;
        }
        let cur = self.audio.default_id();
        let next = match cands.iter().position(|d| Some(&d.id) == cur.as_ref()) {
            Some(i) => (i + 1) % cands.len(),
            None => 0,
        };
        if let Err(e) = self.audio.set_default(&cands[next].id) {
            self.balloon("切换失败", &format!("{}\n{}", cands[next].name, e.message()), NIIF_ERROR);
        }
        self.refresh();
        true
    }

    fn build_menu(&mut self) -> HMENU {
        let devices = self.audio.devices();
        let _ = self.candidates(&devices);
        let cur = self.audio.default_id();
        self.menu_items.clear();
        unsafe {
            let menu = CreatePopupMenu().unwrap();
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("勾选参与轮换的设备"));
            let mut default_cmd = None;
            for d in &devices {
                let cmd = ID_DEVICE_BASE + self.menu_items.len();
                let checked = self.config.devices.iter().any(|e| e.id == d.id);
                let flags = MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED };
                let _ = AppendMenuW(menu, flags, cmd, PCWSTR(wide(&d.name).as_ptr()));
                if Some(&d.id) == cur.as_ref() {
                    default_cmd = Some(cmd);
                }
                self.menu_items.push(Entry { id: d.id.clone(), name: d.name.clone() });
            }
            for e in &self.config.devices {
                if devices.iter().any(|d| d.id == e.id) {
                    continue;
                }
                let cmd = ID_DEVICE_BASE + self.menu_items.len();
                let label = wide(&format!("{}（未连接）", e.name));
                let _ = AppendMenuW(menu, MF_STRING | MF_CHECKED, cmd, PCWSTR(label.as_ptr()));
                self.menu_items.push(e.clone());
            }
            if let Some(cmd) = default_cmd {
                let _ = SetMenuDefaultItem(menu, cmd as u32, 0);
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let auto = if autostart::is_enabled() { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING | auto, ID_AUTOSTART, w!("开机自启"));
            let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_CONFIG, w!("打开配置文件夹"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("退出"));
            menu
        }
    }

    fn on_command(&mut self, cmd: usize) {
        match cmd {
            ID_AUTOSTART => {
                let on = !autostart::is_enabled();
                if !autostart::set(on) {
                    self.balloon("AudioFlip", "无法修改开机自启设置", NIIF_ERROR);
                }
            }
            ID_OPEN_CONFIG => {
                let dir = Config::dir();
                let _ = std::fs::create_dir_all(&dir);
                if !self.config.path.exists() {
                    let _ = self.config.save();
                }
                let p = wide(&dir.to_string_lossy());
                unsafe {
                    ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL);
                }
            }
            ID_EXIT => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            c if c >= ID_DEVICE_BASE => {
                let Some(item) = self.menu_items.get(c - ID_DEVICE_BASE).cloned() else { return };
                if let Some(pos) = self.config.devices.iter().position(|e| e.id == item.id) {
                    self.config.devices.remove(pos);
                } else {
                    self.config.devices.push(item);
                }
                if self.config.save().is_err() {
                    self.balloon("AudioFlip", "配置保存失败", NIIF_ERROR);
                }
                self.refresh();
            }
            _ => {}
        }
    }
}

fn show_menu(hwnd: HWND, x: i32, y: i32) {
    let Some(menu) = with_app(|app| app.build_menu()) else { return };
    unsafe {
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            x,
            y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if cmd.0 != 0 {
            with_app(|app| app.on_command(cmd.0 as usize));
        }
    }
}

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            match event {
                NIN_SELECT_EVT | NIN_KEYSELECT_EVT => {
                    let toggled = with_app(|app| {
                        let now = unsafe { GetTickCount64() };
                        if now.saturating_sub(app.last_click) < CLICK_DEBOUNCE_MS {
                            return true;
                        }
                        app.last_click = now;
                        app.toggle()
                    });
                    if toggled == Some(false) {
                        let (x, y) = cursor();
                        show_menu(hwnd, x, y);
                    }
                }
                WM_CONTEXTMENU => {
                    let x = (wparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    show_menu(hwnd, x, y);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DEVICES_CHANGED => {
            // Coalesce notification bursts (one plug event fires several callbacks).
            unsafe { SetTimer(Some(hwnd), TIMER_REFRESH, 150, None) };
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_REFRESH => {
            // If the state is busy, leave the timer running so it retries.
            if with_app(|app| app.refresh()).is_some() {
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_REFRESH);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            with_app(|app| unsafe {
                app.audio.unregister(&app.notifier);
                let _ = Shell_NotifyIconW(NIM_DELETE, &app.nid());
            });
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.get() => {
            // Explorer restarted: the tray icon is gone and must be re-added.
            with_app(|app| app.add_tray());
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn main() {
    unsafe {
        let _mutex = match CreateMutexW(None, true, w!("Local\\AudioFlip.SingleInstance")) {
            Ok(h) if GetLastError() != ERROR_ALREADY_EXISTS => h,
            _ => return,
        };
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
            return;
        }
        let audio = match Audio::new() {
            Ok(a) => a,
            Err(e) => {
                let msg = wide(&format!("无法初始化音频接口：{}", e.message()));
                MessageBoxW(None, PCWSTR(msg.as_ptr()), w!("AudioFlip"), MB_ICONERROR);
                return;
            }
        };

        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let class = w!("AudioFlipWnd");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        // A hidden top-level window (not HWND_MESSAGE) so it receives the TaskbarCreated broadcast.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("AudioFlip"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )
        .unwrap();
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        TASKBAR_CREATED.set(taskbar_created);
        let _ = ChangeWindowMessageFilterEx(hwnd, taskbar_created, MSGFLT_ALLOW, None);

        let (config, existed) = Config::load();
        let notifier = Notifier::create(hwnd.0 as isize, WM_DEVICES_CHANGED);
        let _ = audio.register(&notifier);
        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                hwnd,
                audio,
                config,
                notifier,
                icons: HashMap::new(),
                warn_icon: warning_icon(),
                last_click: 0,
                menu_items: Vec::new(),
            })
        });
        with_app(|app| {
            app.add_tray();
            if !existed {
                let _ = app.config.save();
                app.balloon(
                    "AudioFlip 已启动",
                    "右键图标勾选要轮换的设备，之后左键单击即可切换。\n建议在 设置 → 个性化 → 任务栏 → 其他系统托盘图标 中开启 AudioFlip。",
                    NIIF_INFO,
                );
            }
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
        CoUninitialize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_endpoint_icons() {
        for spec in [r"%windir%\system32\mmres.dll,-3030", r"%windir%\system32\mmres.dll,-3017"] {
            assert!(load_icon(spec).is_some(), "{spec}");
        }
        assert!(load_icon("bogus").is_none());
        assert!(!warning_icon().is_invalid());
    }

    #[test]
    fn tip_is_truncated_and_terminated() {
        let mut buf = [1u16; 8];
        copy_wstr(&mut buf, "AudioFlip · 很长的设备名");
        assert_eq!(buf[7], 0);
        assert_eq!(String::from_utf16_lossy(&buf[..7]), "AudioFl");
    }
}
