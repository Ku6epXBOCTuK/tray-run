use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "tray-run";

pub fn install(exe: &Path, config: &Path) -> Result<(), String> {
    let data = format!("\"{}\" --config \"{}\"", exe.display(), config.display());
    let data_w = to_wide(&data);
    let name_w = to_wide(VALUE_NAME);
    unsafe {
        let key = open_key(KEY_SET_VALUE)?;
        let status = RegSetValueExW(
            key,
            name_w.as_ptr(),
            0,
            REG_SZ,
            data_w.as_ptr() as *const u8,
            (data_w.len() * 2) as u32,
        );
        RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return Err(format!("RegSetValueExW failed (error {status})"));
        }
    }
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let name_w = to_wide(VALUE_NAME);
    unsafe {
        let key = open_key(KEY_SET_VALUE)?;
        let status = RegDeleteValueW(key, name_w.as_ptr());
        RegCloseKey(key);
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return Err(format!("RegDeleteValueW failed (error {status})"));
        }
    }
    Ok(())
}

pub fn is_installed() -> bool {
    unsafe {
        let key = match open_key(KEY_QUERY_VALUE) {
            Ok(key) => key,
            Err(_) => return false,
        };
        let mut size: u32 = 0;
        let name_w = to_wide(VALUE_NAME);
        let status = RegQueryValueExW(
            key,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        );
        RegCloseKey(key);
        status == ERROR_SUCCESS
    }
}

unsafe fn open_key(access: u32) -> Result<HKEY, String> {
    let path = to_wide(RUN_KEY);
    let mut key: HKEY = std::ptr::null_mut();
    let status = RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut key);
    if status != ERROR_SUCCESS {
        return Err(format!("RegOpenKeyExW failed (error {status})"));
    }
    Ok(key)
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
