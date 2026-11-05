use std::mem;
use std::path::Path;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HWND, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, OpenProcess, ResumeThread, WaitForSingleObject,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, STARTF_USESHOWWINDOW, STARTUPINFOW,
};
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
    SetWindowTextW, ShowWindow, SW_HIDE, SW_SHOWNORMAL,
};

const CONHOST: &str = "C:\\Windows\\System32\\conhost.exe";
const CREATE_NEW_CONSOLE: PROCESS_CREATION_FLAGS = 0x0000_0010;
const CREATE_SUSPENDED: PROCESS_CREATION_FLAGS = 0x0000_0004;
const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;

pub struct ConsoleRunner {
    pub visible: bool,
    pid: u32,
    title: String,
    hwnd: HWND,
    client: Option<HANDLE>,
    job: HANDLE,
    proc: HANDLE,
}

pub enum StartError {
    Exited(u32),
    Failed(String),
}

impl ConsoleRunner {
    pub fn spawn(
        name: &str,
        command: &str,
        dir: Option<&Path>,
        start_hidden: bool,
    ) -> Result<Self, StartError> {
        let (program, args) = split_command(command).map_err(StartError::Failed)?;
        let cmdline = format!("\"{}\" {}", CONHOST, quote_args(&program, &args));
        let mut si: STARTUPINFOW = unsafe { mem::zeroed() };
        si.cb = mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_HIDE as u16;
        let mut pi: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        let mut wide: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
        let exe = to_wide(CONHOST);
        let cwd = dir.map(|p| to_wide(&p.to_string_lossy()));
        let ok = unsafe {
            CreateProcessW(
                exe.as_ptr(),
                wide.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                CREATE_NEW_CONSOLE | CREATE_SUSPENDED,
                std::ptr::null_mut(),
                cwd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                &si,
                &mut pi,
            )
        };
        if ok == 0 {
            let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            return Err(StartError::Failed(format!(
                "CreateProcessW failed (error {err}): {command}"
            )));
        }
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

        let mut runner = ConsoleRunner {
            visible: false,
            pid: pi.dwProcessId,
            title: name.to_string(),
            hwnd: std::ptr::null_mut(),
            client: None,
            job,
            proc: pi.hProcess,
        };
        runner.client = find_child_handle(runner.pid, Duration::from_millis(600));
        match find_console_window(runner.pid, Duration::from_secs(3)) {
            Some(hwnd) => runner.attach(hwnd),
            None => {
                let exited = unsafe { WaitForSingleObject(pi.hProcess, 0) } == WAIT_OBJECT_0;
                let mut code: u32 = 0;
                if exited {
                    unsafe { GetExitCodeProcess(pi.hProcess, &mut code) };
                }
                runner.kill();
                if exited {
                    return Err(StartError::Exited(code));
                }
                return Err(StartError::Failed(format!(
                    "console window not found for {name} (is it a console app?)"
                )));
            }
        }
        if start_hidden {
            runner.hide();
        } else {
            runner.show();
        }
        Ok(runner)
    }

    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        let title = to_wide(&self.title);
        unsafe {
            SetWindowTextW(hwnd, title.as_ptr());
            self.visible = IsWindow(hwnd) != 0 && is_window_visible(hwnd);
        }
    }

    pub fn show(&mut self) {
        unsafe {
            ShowWindow(self.hwnd, SW_SHOWNORMAL);
            SetForegroundWindow(self.hwnd);
        }
        self.visible = true;
    }

    pub fn hide(&mut self) {
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

    pub fn ensure_window(&mut self) {
        if !self.hwnd.is_null() && unsafe { IsWindow(self.hwnd) } != 0 {
            return;
        }
        if let Some(hwnd) = find_console_window(self.pid, Duration::from_millis(300)) {
            let visible = self.visible;
            self.attach(hwnd);
            if visible {
                self.show();
            } else {
                self.hide();
            }
        }
    }

    pub fn poll_exit(&self) -> Option<u32> {
        let handle = self.client.unwrap_or(self.proc);
        if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code: u32 = 0;
        unsafe { GetExitCodeProcess(handle, &mut code) };
        Some(code)
    }

    pub fn kill(&self) {
        unsafe { TerminateJobObject(self.job, 1) };
    }
}

impl Drop for ConsoleRunner {
    fn drop(&mut self) {
        unsafe {
            if let Some(client) = self.client.take() {
                CloseHandle(client);
            }
            CloseHandle(self.proc);
            CloseHandle(self.job);
        }
    }
}

pub fn is_user_stop(code: u32) -> bool {
    code == STATUS_CONTROL_C_EXIT
}

fn split_command(command: &str) -> Result<(String, Vec<String>), String> {
    if command.trim().is_empty() {
        return Err("command is empty".to_string());
    }
    let wide = to_wide(command);
    let mut argc: i32 = 0;
    let argv = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
    if argv.is_null() || argc == 0 {
        return Err(format!("cannot parse command {command:?}"));
    }
    let mut parts = Vec::with_capacity(argc as usize);
    for index in 0..argc as usize {
        let ptr = unsafe { *argv.add(index) };
        let len = unsafe { wide_len(ptr) };
        parts.push(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(ptr, len)
        }));
    }
    unsafe { LocalFree(argv as *mut core::ffi::c_void) };
    let program = parts.remove(0);
    Ok((program, parts))
}

unsafe fn wide_len(ptr: *const u16) -> usize {
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    len
}

fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

fn quote_args(program: &str, args: &[String]) -> String {
    let mut out = quote_arg(program);
    for arg in args {
        out.push(' ');
        out.push_str(&quote_arg(arg));
    }
    out
}

pub fn join_args(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| quote_arg(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn is_window_visible(hwnd: HWND) -> bool {
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) != 0 }
}

fn find_console_window(pid: u32, timeout: Duration) -> Option<HWND> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = std::cell::Cell::new((pid, 0usize));
        let lparam = &state as *const _ as isize;
        unsafe { EnumWindows(Some(enum_proc), lparam) };
        let (_, hwnd) = state.get();
        if hwnd != 0 {
            return Some(hwnd as HWND);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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

fn find_child_handle(parent_pid: u32, timeout: Duration) -> Option<HANDLE> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = child_of(parent_pid) {
            let handle = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            };
            if !handle.is_null() {
                return Some(handle);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn child_of(parent_pid: u32) -> Option<u32> {
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap.is_null() {
        return None;
    }
    let mut found = None;
    unsafe {
        let mut entry: PROCESSENTRY32W = mem::zeroed();
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                if entry.th32ParentProcessID == parent_pid && entry.th32ProcessID != 0 {
                    found = Some(entry.th32ProcessID);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_command() {
        let (program, args) = split_command(r#""C:\Program Files\node.exe" run "a b""#).unwrap();
        assert_eq!(program, r"C:\Program Files\node.exe");
        assert_eq!(args, vec!["run".to_string(), "a b".to_string()]);
    }

    #[test]
    fn rounds_trips_quotes() {
        let (program, args) = split_command("npm  run dev").unwrap();
        assert_eq!(quote_args(&program, &args), "npm run dev");
    }

    #[test]
    fn rejects_blank_command() {
        assert!(split_command("  ").is_err());
    }
}
