use crossbeam_channel::{Receiver, Sender};
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos,
    GetMessageW, LoadIconW, ModifyMenuW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, IDI_APPLICATION, MF_BYCOMMAND,
    MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN, WM_APP, WM_COMMAND, WM_DESTROY,
    WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};

pub enum Action {
    Toggle,
    Quit,
}

const WM_TRAYICON: u32 = WM_APP + 1;
const ID_TOGGLE: usize = 1001;
const ID_QUIT: usize = 1002;

pub fn dlog(msg: &str) {
    if let Some(p) = std::env::var_os("TRAY_RUN_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
        {
            use std::io::Write;
            let pid = std::process::id();
            let tid = std::thread::current().id();
            let _ = writeln!(f, "[pid={pid} tid={tid:?}] {msg}");
        }
    }
}

struct Shared {
    tx: Sender<Action>,
    menu: isize,
    visible: bool,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

static SHARED: std::sync::Mutex<Option<Shared>> = std::sync::Mutex::new(None);

pub struct Tray {
    thread: Option<std::thread::JoinHandle<()>>,
    rx: Receiver<Action>,
}

impl Tray {
    pub fn new(name: &str) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let name_w: Vec<u16> = name.encode_utf16().collect();
        dlog("tray: starting thread");
        let thread = std::thread::Builder::new()
            .spawn(move || unsafe { run_tray_thread(name_w, tx) })
            .expect("failed to spawn tray thread");
        Tray {
            thread: Some(thread),
            rx,
        }
    }

    pub fn poll(&self) -> Option<Action> {
        self.rx.try_recv().ok()
    }

    pub fn set_toggle_label(&self, visible: bool) {
        let mut guard = SHARED.lock().unwrap();
        if let Some(s) = guard.as_mut() {
            if s.visible == visible {
                return;
            }
            s.visible = visible;
            let label = if visible {
                "Hide console"
            } else {
                "Show console"
            };
            let text: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                ModifyMenuW(
                    s.menu as _,
                    ID_TOGGLE as u32,
                    MF_BYCOMMAND | MF_STRING,
                    ID_TOGGLE,
                    text.as_ptr(),
                );
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // request exit: WM_NULL won't quit; simplest is leaving the thread
        // daemon-like: process exit will clean up. Still detach the handle.
        self.thread.take();
    }
}

unsafe fn run_tray_thread(name_w: Vec<u16>, tx: Sender<Action>) {
    let hinstance = get_instance_handle();
    let class_name: Vec<u16> = "tray_run_traywnd\0".encode_utf16().collect();
    let wnd_class = WNDCLASSW {
        lpfnWndProc: Some(tray_proc),
        lpszClassName: class_name.as_ptr(),
        hInstance: hinstance,
        ..std::mem::zeroed()
    };
    let atom = RegisterClassW(&wnd_class);
    dlog(&format!("tray: RegisterClassW atom={atom}"));
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
        hinstance,
        std::ptr::null(),
    );
    dlog(&format!(
        "tray: hwnd={hwnd:?} err={}",
        std::io::Error::last_os_error()
    ));
    if hwnd.is_null() {
        dlog("tray: CreateWindowExW failed");
        return;
    }

    let menu = CreatePopupMenu();
    dlog(&format!("tray: menu={menu:?}"));
    let toggle_text: Vec<u16> = "Hide console\0".encode_utf16().collect();
    AppendMenuW(menu, MF_STRING, ID_TOGGLE, toggle_text.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    let quit_text: Vec<u16> = "Quit\0".encode_utf16().collect();
    AppendMenuW(menu, MF_STRING, ID_QUIT, quit_text.as_ptr());

    *SHARED.lock().unwrap() = Some(Shared {
        tx,
        menu: menu as isize,
        visible: true,
    });

    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = unsafe { LoadIconW(0 as HINSTANCE, IDI_APPLICATION) };
    let tip_len = name_w.len().min(nid.szTip.len() - 1);
    nid.szTip[..tip_len].copy_from_slice(&name_w[..tip_len]);
    let ok = Shell_NotifyIconW(NIM_ADD, &nid);
    dlog(&format!("tray: Shell_NotifyIconW(NIM_ADD)={ok}"));
    if ok == 0 {
        dlog("tray: Shell_NotifyIconW failed");
        *SHARED.lock().unwrap() = None;
        return;
    }

    let mut msg = std::mem::zeroed();
    dlog("tray: entering message loop");
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    dlog("tray: message loop exited");
    Shell_NotifyIconW(NIM_DELETE, &nid);
    dlog("tray: icon deleted");
}

unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    dlog(&format!(
        "tray: wndproc msg=0x{msg:x} hwnd={hwnd:?} wp={wparam:x} lp={lparam:x}"
    ));
    match msg {
        WM_TRAYICON => {
            let event = lparam as u32;
            dlog(&format!("tray: WM_TRAYICON event=0x{event:x}"));
            if event == WM_LBUTTONUP {
                dlog("tray: left click -> Toggle");
                if let Some(s) = SHARED.lock().unwrap().as_ref() {
                    let _ = s.tx.send(Action::Toggle);
                }
            } else if event == WM_RBUTTONUP {
                dlog("tray: right click -> show menu");
                let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
                let _ = GetCursorPos(&mut pt);
                SetForegroundWindow(hwnd);
                if let Some(s) = SHARED.lock().unwrap().as_ref() {
                    TrackPopupMenu(
                        s.menu as _,
                        TPM_LEFTALIGN | TPM_BOTTOMALIGN,
                        pt.x,
                        pt.y,
                        0,
                        hwnd,
                        std::ptr::null(),
                    );
                }
                PostMessageW(hwnd, WM_NULL, 0, 0);
            }
            0
        }
        WM_COMMAND => {
            let id = (wparam as usize) & 0xffff;
            dlog(&format!("tray: WM_COMMAND id={id}"));
            if let Some(s) = SHARED.lock().unwrap().as_ref() {
                match id {
                    ID_TOGGLE => {
                        let _ = s.tx.send(Action::Toggle);
                    }
                    ID_QUIT => {
                        let _ = s.tx.send(Action::Quit);
                    }
                    _ => {}
                }
            }
            0
        }
        WM_DESTROY => {
            dlog("tray: WM_DESTROY");
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn get_instance_handle() -> windows_sys::Win32::Foundation::HMODULE {
    unsafe { GetModuleHandleW(std::ptr::null()) }
}
