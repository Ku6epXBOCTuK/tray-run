use crossbeam_channel::{Receiver, Sender};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, LoadIconW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, MF_POPUP, MF_SEPARATOR, MF_STRING,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_NULL,
    WM_RBUTTONUP, WNDCLASSW,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Toggle(u64),
    Restart(u64),
    Stop(u64),
    Start(u64),
    Reload,
    ToggleAutostart,
    QuitAll,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppView {
    pub id: u64,
    pub label: String,
    pub running: bool,
    pub visible: bool,
}

const WM_TRAYICON: u32 = WM_APP + 1;
const WM_APP_STATE: u32 = WM_APP + 2;

const ID_RELOAD: i32 = 1;
const ID_AUTOSTART: i32 = 2;
const ID_QUIT: i32 = 3;
const APP_BASE: i32 = 1000;
const APP_STRIDE: i32 = 8;

const ACT_TOGGLE: i32 = 0;
const ACT_RESTART: i32 = 1;
const ACT_STOP: i32 = 2;
const ACT_START: i32 = 3;

pub fn dlog(msg: &str) {
    if let Some(path) = std::env::var_os("TRAY_RUN_LOG") {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = writeln!(file, "[pid={}] {msg}", std::process::id());
        }
    }
}

struct Shared {
    tx: Sender<Action>,
    views: Vec<AppView>,
    autostart: bool,
    nid: NOTIFYICONDATAW,
}

unsafe impl Send for Shared {}

static SHARED: std::sync::Mutex<Option<Shared>> = std::sync::Mutex::new(None);

pub struct Tray {
    rx: Receiver<Action>,
    last_key: std::cell::RefCell<String>,
}

impl Tray {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("tray".to_string())
            .spawn(move || unsafe { run_tray_thread(tx) })
            .expect("failed to spawn tray thread");
        Tray {
            rx,
            last_key: std::cell::RefCell::new(String::new()),
        }
    }

    pub fn poll(&self) -> Option<Action> {
        self.rx.try_recv().ok()
    }

    pub fn sync(&self, views: Vec<AppView>, autostart: bool) {
        let key = format!("{autostart}|{views:?}");
        {
            let mut last = self.last_key.borrow_mut();
            if *last == key {
                return;
            }
            last.clear();
            last.push_str(&key);
        }
        let hwnd = {
            let mut guard = SHARED.lock().unwrap();
            match guard.as_mut() {
                Some(shared) => {
                    shared.views = views;
                    shared.autostart = autostart;
                    shared.nid.hWnd
                }
                None => return,
            }
        };
        unsafe { PostMessageW(hwnd, WM_APP_STATE, 0, 0) };
    }
}

unsafe fn run_tray_thread(tx: Sender<Action>) {
    let instance = GetModuleHandleW(std::ptr::null());
    let class_name: Vec<u16> = "tray_run_traywnd\0".encode_utf16().collect();
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(tray_proc),
        lpszClassName: class_name.as_ptr(),
        hInstance: instance,
        ..std::mem::zeroed()
    };
    let atom = RegisterClassW(&window_class);
    if atom == 0 {
        dlog("tray: RegisterClassW failed");
        return;
    }
    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        std::ptr::null(),
        0,
        0,
        0,
        0,
        0,
        windows_sys::Win32::UI::WindowsAndMessaging::HWND_MESSAGE,
        std::ptr::null_mut(),
        instance,
        std::ptr::null(),
    );
    if hwnd.is_null() {
        dlog(&format!(
            "tray: CreateWindowExW failed: {}",
            std::io::Error::last_os_error()
        ));
        return;
    }

    let icon = LoadIconW(instance, std::ptr::without_provenance::<u16>(1));
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = icon;
    write_tip(&mut nid, "tray-run");

    if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
        dlog("tray: Shell_NotifyIconW(NIM_ADD) failed");
        return;
    }
    dlog("tray: icon added");

    *SHARED.lock().unwrap() = Some(Shared {
        tx,
        views: Vec::new(),
        autostart: false,
        nid,
    });

    let mut message = std::mem::zeroed();
    while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

fn write_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let len = wide.len().min(nid.szTip.len() - 1);
    nid.szTip = [0; 128];
    nid.szTip[..len].copy_from_slice(&wide[..len]);
}

fn tooltip_text(views: &[AppView]) -> String {
    match views {
        [] => "tray-run: no apps".to_string(),
        [one] => one.label.clone(),
        _ => {
            let running = views.iter().filter(|v| v.running).count();
            format!("tray-run ({running}/{})", views.len())
        }
    }
}

unsafe fn popup_menu(hwnd: HWND) {
    let (views, autostart) = {
        let guard = SHARED.lock().unwrap();
        match guard.as_ref() {
            Some(shared) => (shared.views.clone(), shared.autostart),
            None => return,
        }
    };

    let mut point = POINT { x: 0, y: 0 };
    GetCursorPos(&mut point);
    SetForegroundWindow(hwnd);

    let menu = CreatePopupMenu();
    for (slot, view) in views.iter().enumerate() {
        let submenu = CreatePopupMenu();
        let base = APP_BASE + slot as i32 * APP_STRIDE;
        if view.running {
            let toggle = if view.visible {
                "Hide console"
            } else {
                "Show console"
            };
            append(submenu, base + ACT_TOGGLE, toggle);
            append(submenu, base + ACT_RESTART, "Restart");
            append(submenu, base + ACT_STOP, "Stop");
        } else {
            append(submenu, base + ACT_START, "Start");
        }
        let label = to_wide(&view.label);
        AppendMenuW(menu, MF_STRING | MF_POPUP, submenu as usize, label.as_ptr());
    }

    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    append(menu, ID_RELOAD, "Reload config");
    append(
        menu,
        ID_AUTOSTART,
        if autostart {
            "Remove autostart"
        } else {
            "Install autostart"
        },
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    append(menu, ID_QUIT, "Quit");

    let command = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
        point.x,
        point.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    DestroyMenu(menu);
    PostMessageW(hwnd, WM_NULL, 0, 0);

    if command > 0 {
        dispatch(command, &views);
    }
}

unsafe fn append(menu: windows_sys::Win32::UI::WindowsAndMessaging::HMENU, id: i32, text: &str) {
    let wide = to_wide(text);
    AppendMenuW(menu, MF_STRING, id as usize, wide.as_ptr());
}

fn dispatch(command: i32, views: &[AppView]) {
    let action = if command >= APP_BASE {
        let offset = command - APP_BASE;
        let slot = (offset / APP_STRIDE) as usize;
        let kind = offset % APP_STRIDE;
        let Some(view) = views.get(slot) else {
            return;
        };
        match kind {
            ACT_TOGGLE => Action::Toggle(view.id),
            ACT_RESTART => Action::Restart(view.id),
            ACT_STOP => Action::Stop(view.id),
            ACT_START => Action::Start(view.id),
            _ => return,
        }
    } else {
        match command {
            ID_RELOAD => Action::Reload,
            ID_AUTOSTART => Action::ToggleAutostart,
            ID_QUIT => Action::QuitAll,
            _ => return,
        }
    };
    if let Some(shared) = SHARED.lock().unwrap().as_ref() {
        let _ = shared.tx.send(action);
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event = lparam as u32;
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                popup_menu(hwnd);
            }
            0
        }
        WM_APP_STATE => {
            let mut guard = SHARED.lock().unwrap();
            if let Some(shared) = guard.as_mut() {
                let text = tooltip_text(&shared.views);
                write_tip(&mut shared.nid, &text);
                shared.nid.uFlags = NIF_TIP;
                Shell_NotifyIconW(NIM_MODIFY, &shared.nid);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
