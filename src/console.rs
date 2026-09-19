use std::mem;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, ResumeThread, WaitForSingleObject, PROCESS_CREATION_FLAGS,
    PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, SetForegroundWindow, SetWindowTextW,
    ShowWindow, SW_HIDE, SW_SHOWNORMAL,
};

const CONHOST: &str = "C:\\Windows\\System32\\conhost.exe";
const CREATE_NEW_CONSOLE: PROCESS_CREATION_FLAGS = 0x0000_0010;
const CREATE_SUSPENDED: PROCESS_CREATION_FLAGS = 0x0000_0004;

pub struct ConsoleRunner {
    pub name: String,
    pub visible: bool,
    hwnd: HWND,
    job: HANDLE,
    proc: HANDLE,
}

macro_rules! dlog {
    ($($arg:tt)*) => {
        if std::env::var_os("TRAY_RUN_LOG").is_some() {
            let mut f = std::fs::OpenOptions::new().create(true).append(true)
                .open(std::env::var("TRAY_RUN_LOG").unwrap()).ok();
            if let Some(f) = f.as_mut() {
                use std::io::Write;
                let _ = writeln!(f, $($arg)*);
            }
        }
    };
}

impl ConsoleRunner {
    pub fn spawn(name: &str, program: &str, args: &[String]) -> Result<Self, String> {
        let cmdline = format!("\"{}\" {}", CONHOST, quote_args(program, args));
        let mut si: STARTUPINFOW = unsafe { mem::zeroed() };
        si.cb = mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = 0; // SW_HIDE
        let mut pi: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        let mut wide: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
        let exe = to_wide(CONHOST);
        let ok = unsafe {
            CreateProcessW(
                exe.as_ptr() as *mut u16,
                wide.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                CREATE_NEW_CONSOLE | CREATE_SUSPENDED,
                std::ptr::null_mut(),
                std::ptr::null(),
                &si,
                &mut pi,
            )
        };
        if ok == 0 {
            let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            dlog!("CreateProcessW failed: {err}");
            return Err(format!("CreateProcessW failed (error {err})"));
        }
        dlog!(
            "spawned conhost pid={} thread={:p}",
            pi.dwProcessId,
            pi.hThread
        );
        unsafe {
            ResumeThread(pi.hThread);
            CloseHandle(pi.hThread);
        }

        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            AssignProcessToJobObject(job, pi.hProcess);
        }

        let hwnd = match find_console_window(pi.dwProcessId) {
            Some(h) => {
                dlog!("window found hwnd={:p}", h);
                h
            }
            None => {
                dlog!("window NOT found in 3s");
                return Err("console window not found (is it a console app?)".to_string());
            }
        };

        let mut runner = ConsoleRunner {
            name: name.to_string(),
            visible: false,
            hwnd,
            job,
            proc: pi.hProcess,
        };
        let title = to_wide(name);
        unsafe { SetWindowTextW(hwnd, title.as_ptr()) };
        runner.hide();
        Ok(runner)
    }

    pub fn show(&mut self) {
        dlog!("show() called for hwnd={:p}", self.hwnd);
        unsafe {
            ShowWindow(self.hwnd, SW_SHOWNORMAL);
            SetForegroundWindow(self.hwnd);
        }
        self.visible = true;
    }

    pub fn hide(&mut self) {
        dlog!("hide() called for hwnd={:p}", self.hwnd);
        unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        self.visible = false;
    }

    pub fn toggle(&mut self) {
        if self.visible {
            self.hide();
        } else {
            self.show();
        }
    }

    /// None = running; Some(code) = console host exited (child gone with it)
    pub fn poll_exit(&self) -> Option<u32> {
        let wait = unsafe { WaitForSingleObject(self.proc, 0) };
        if wait == WAIT_OBJECT_0 {
            let mut code: u32 = 0;
            unsafe { GetExitCodeProcess(self.proc, &mut code) };
            dlog!("conhost exited code={code}");
            Some(code)
        } else {
            None
        }
    }

    pub fn kill(&self) {
        unsafe { TerminateJobObject(self.job, 1) };
    }
}

impl Drop for ConsoleRunner {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.proc);
            CloseHandle(self.job);
        }
    }
}

fn quote_args(program: &str, args: &[String]) -> String {
    let mut s = String::new();
    for a in std::iter::once(program).chain(args.iter().map(|s| s.as_str())) {
        if !s.is_empty() {
            s.push(' ');
        }
        if a.contains(' ') || a.contains('"') {
            s.push('"');
            for c in a.chars() {
                if c == '"' {
                    s.push('\\');
                }
                s.push(c);
            }
            s.push('"');
        } else {
            s.push_str(a);
        }
    }
    s
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn find_console_window(conhost_pid: u32) -> Option<HWND> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let state = std::cell::Cell::new((conhost_pid, 0usize));
        let lparam = &state as *const _ as isize;
        unsafe { EnumWindows(Some(enum_proc), lparam) };
        let (_, hwnd) = state.get();
        if hwnd != 0 {
            return Some(hwnd as HWND);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: isize) -> i32 {
    let cell = &*(lparam as *const std::cell::Cell<(u32, usize)>);
    let (conhost_pid, _) = cell.get();
    let mut wpid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut wpid);
    if wpid == 0 {
        return 1;
    }
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, buf.as_mut_ptr(), 64);
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if class == "ConsoleWindowClass" && pid_belongs_to(wpid, conhost_pid) {
        cell.set((conhost_pid, hwnd as usize));
        return 0;
    }
    1
}

/// True if the window-owning process is the conhost we spawned, or its direct
/// child (the client app itself).
fn pid_belongs_to(window_pid: u32, conhost_pid: u32) -> bool {
    if window_pid == conhost_pid {
        return true;
    }
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap.is_null() {
        return false;
    }
    let mut found = false;
    unsafe {
        let mut entry: PROCESSENTRY32W = mem::zeroed();
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == window_pid {
                    found = entry.th32ParentProcessID == conhost_pid;
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    found
}
