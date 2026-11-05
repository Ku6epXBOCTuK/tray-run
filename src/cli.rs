use std::io::Write;

use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};

pub const MB_ICONINFO: u32 = 0x40;
pub const MB_ICONWARNING: u32 = 0x30;
pub const MB_ICONERROR: u32 = 0x10;

pub const TEXT: &str = r#"tray-run
Run console commands hidden and control them from one tray icon.

USAGE
  tray-run.exe                            start every app from the config file
  tray-run.exe --config PATH              start every app from PATH
  tray-run.exe --install-startup          add the autostart registry entry
  tray-run.exe --uninstall-startup        remove the autostart registry entry
  tray-run.exe --help                     print this help (also -h, -?, /?)
  tray-run.exe --version                  print the version

  An unknown option prints this usage and exits with code 2.

  tray-run.exe is a GUI process and has no console of its own. Help and errors
  are written to the console that started it, or shown in a message box when
  it is started by double click.

BEHAVIOUR
  Each command runs in a real console window (conhost.exe) created hidden, so
  output, selection and copy keep working. The tray icon has no window of its
  own: left and right click both open the same menu.

CONFIG FILE
  Lookup order, first match wins:
    1. --config PATH
    2. environment variable TRAY_RUN_CONFIG
    3. %APPDATA%\tray-run\config.toml
    4. tray-run.toml next to tray-run.exe
  Menu order follows the order of [app.*] sections. Example:

    [defaults]
    restart = "on-crash"            # never | on-crash | always
    start_hidden = true
    restart_gap_ms = 2000           # base delay before a restart
    restart_backoff_max_ms = 60000  # delay ceiling
    restart_max = 5                 # crashes in a row before giving up, 0 = no limit
    restart_reset_ms = 60000        # uptime that resets the crash counter

    [app.dev-server]
    command = "npm run dev"         # required
    dir = "C:/projects/web"         # working directory, default: the cwd tray-run was started from
    restart = "always"              # overrides [defaults]
    start_hidden = false

  [defaults] applies to every app; keys in [app.*] except command and dir
  override it. An unknown key is a config error, so a typo never passes
  silently. command is parsed with Windows command line rules: quote paths that
  contain spaces, as in a .bat file:

    command = "\"C:\\Program Files\\nodejs\\node.exe\" server.js"

TRAY MENU
  app   Show console / Hide console   toggle the hidden console window
  app   Restart                       kill and start again
  app   Stop                          kill and keep it stopped
  app   Start                         the app is not running
  name -- stopped                     the app is not running
  name -- gave up after N crashes     restarts exhausted, Start is manual
  name (N restarts, next in Ns)       waiting for the next restart
  Reload config                       re-read the file: new apps start,
                                      removed stop, changed restart,
                                      unchanged keep running
  Install autostart / Remove autostart
  Quit                                stop every app and exit

  Closing a console window by hand counts as an intentional stop: the app is
  marked stopped and does not come back by itself.

RESTARTS
  Only as far as the restart policy allows. An app that stays up for
  restart_reset_ms clears its crash counter. The delay doubles from
  restart_gap_ms up to restart_backoff_max_ms. After restart_max crashes the
  app is given up until Start is used from the menu.

AUTOSTART
  --install-startup writes one value named tray-run into
  HKCU\Software\Microsoft\Windows\CurrentVersion\Run, pointing at this
  executable and the resolved config. The tray menu does the same.

LOGGING
  Set TRAY_RUN_LOG to a file path to log starts, exits and restarts:

    set TRAY_RUN_LOG=%TEMP%\tray-run.log
    tray-run.exe

EXIT AND SINGLE INSTANCE
  Only one tray-run.exe runs at a time: a second instance reports an error and
  exits. Reload config from the tray menu to pick up edits. Quit stops every app
  and exits.

LIMITATIONS
  Windows only. A command that exits before its window is found is reported by
  exit code and never shows a window.
"#;

pub fn emit(text: &str, icon: u32) {
    let mut text = text.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if write_stdout(&text) || write_console(&text) {
        return;
    }
    message_box(text.trim_end(), icon);
}

fn write_stdout(text: &str) -> bool {
    let mut out = std::io::stdout();
    out.write_all(text.as_bytes()).is_ok() && out.flush().is_ok()
}

fn write_console(text: &str) -> bool {
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return false;
    }
    match std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        Ok(mut out) => out.write_all(text.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

pub fn message_box(text: &str, icon: u32) {
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let title: Vec<u16> = "tray-run\0".encode_utf16().collect();
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            icon,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_stays_ascii() {
        assert!(
            TEXT.is_ascii(),
            "help must survive legacy console code pages"
        );
    }

    #[test]
    fn help_names_every_entry_point() {
        for needle in [
            "--config",
            "--install-startup",
            "--uninstall-startup",
            "--help",
            "--version",
            "TRAY_RUN_CONFIG",
            "TRAY_RUN_LOG",
            "restart_reset_ms",
            "start_hidden",
        ] {
            assert!(TEXT.contains(needle), "help does not mention {needle}");
        }
    }
}
