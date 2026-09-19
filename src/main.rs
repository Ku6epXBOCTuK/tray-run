#![windows_subsystem = "windows"]

mod console;
mod tray;

use std::time::Duration;
use tray::Action;

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let (name, program, args) = match parse_cli(&raw) {
        Some(v) => v,
        None => fatal("usage: tray-run [--name NAME] [--] <program> [args...]"),
    };
    tray::dlog("main: starting");
    let mut runner = match console::ConsoleRunner::spawn(&name, &program, &args) {
        Ok(r) => r,
        Err(e) => fatal(&e),
    };
    let tray = tray::Tray::new(&name);
    let mut visible = false;
    loop {
        while let Some(action) = tray.poll() {
            match action {
                Action::Toggle => {
                    runner.toggle();
                    visible = !visible;
                    tray.set_toggle_label(visible);
                }
                Action::Quit => {
                    runner.kill();
                    tray::dlog("main: quit");
                    return;
                }
            }
        }
        if runner.poll_exit().is_some() {
            tray::dlog("main: child exited");
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    tray::dlog("main: exiting");
}

fn parse_cli(raw: &[String]) -> Option<(String, String, Vec<String>)> {
    let mut i = 0;
    let mut name: Option<String> = None;
    if raw.first().map(String::as_str) == Some("--name") {
        name = raw.get(1).cloned();
        i = 2;
    }
    let rest = &raw[i..];
    let rest = if rest.first().map(String::as_str) == Some("--") {
        &rest[1..]
    } else {
        rest
    };
    let (program, args) = rest.split_first()?;
    let name = name.unwrap_or_else(|| {
        std::path::Path::new(program)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string())
    });
    Some((name, program.clone(), args.to_vec()))
}

fn fatal(msg: &str) -> ! {
    unsafe {
        let text: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
        let title: Vec<u16> = "tray-run\0".encode_utf16().collect();
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            0x10, // MB_ICONERROR
        );
    }
    std::process::exit(1);
}
