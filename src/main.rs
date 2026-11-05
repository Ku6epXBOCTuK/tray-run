#![windows_subsystem = "windows"]

mod cli;
mod config;
mod console;
mod startup;
mod tray;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use config::{AppConfig, Config, Restart, Settings};
use console::{ConsoleRunner, StartError};
use tray::{Action, AppView};

const TICK: Duration = Duration::from_millis(100);

enum Status {
    Running,
    Stopped,
    Backoff { retry_at: Instant },
    GivenUp,
}

struct Instance {
    id: u64,
    cfg: AppConfig,
    status: Status,
    visible: bool,
    restarts: u32,
    started_at: Option<Instant>,
    runner: Option<ConsoleRunner>,
}

impl Instance {
    fn new(id: u64, cfg: AppConfig) -> Self {
        Instance {
            id,
            cfg,
            status: Status::Stopped,
            visible: false,
            restarts: 0,
            started_at: None,
            runner: None,
        }
    }

    fn start(&mut self) -> Result<(), StartError> {
        let runner = ConsoleRunner::spawn(
            &self.cfg.name,
            &self.cfg.command,
            self.cfg.dir.as_deref(),
            self.cfg.start_hidden,
        )?;
        self.visible = runner.visible;
        self.runner = Some(runner);
        self.status = Status::Running;
        self.started_at = Some(Instant::now());
        Ok(())
    }

    fn start_or_fail(&mut self, settings: &Settings, now: Instant) {
        match self.start() {
            Ok(()) => tray::dlog(&format!("{}: started", self.cfg.name)),
            Err(StartError::Exited(code)) => self.handle_exit(code, settings, now),
            Err(StartError::Failed(err)) => {
                tray::dlog(&format!("{}: {err}", self.cfg.name));
                self.fail(settings, now);
            }
        }
    }

    fn handle_exit(&mut self, code: u32, settings: &Settings, now: Instant) {
        self.runner = None;
        self.visible = false;
        tray::dlog(&format!("{}: exited code=0x{code:08X}", self.cfg.name));
        if console::is_user_stop(code) {
            self.status = Status::Stopped;
            self.restarts = 0;
            return;
        }
        let crash = match self.cfg.restart {
            Restart::Never => false,
            Restart::OnCrash => code != 0,
            Restart::Always => true,
        };
        if crash {
            self.fail(settings, now);
        } else {
            self.status = Status::Stopped;
        }
    }

    fn stop(&mut self) {
        if let Some(runner) = self.runner.take() {
            runner.kill();
        }
        self.visible = false;
        self.status = Status::Stopped;
    }

    fn fail(&mut self, settings: &Settings, now: Instant) {
        self.restarts = self.restarts.saturating_add(1);
        if settings.restart_max > 0 && self.restarts >= settings.restart_max {
            tray::dlog(&format!(
                "{}: giving up after {} failures",
                self.cfg.name, self.restarts
            ));
            self.status = Status::GivenUp;
        } else {
            let delay = backoff(self.restarts, settings);
            tray::dlog(&format!(
                "{}: restart #{} in {} ms",
                self.cfg.name,
                self.restarts,
                delay.as_millis()
            ));
            self.status = Status::Backoff {
                retry_at: now + delay,
            };
        }
    }

    fn tick(&mut self, settings: &Settings, now: Instant) {
        if let Status::Backoff { retry_at } = self.status {
            if now >= retry_at {
                self.start_or_fail(settings, now);
            }
            return;
        }
        if !matches!(self.status, Status::Running) {
            return;
        }
        let exit = self.runner.as_ref().and_then(ConsoleRunner::poll_exit);
        if let Some(code) = exit {
            self.handle_exit(code, settings, now);
            return;
        }
        if let Some(runner) = self.runner.as_mut() {
            runner.ensure_window();
            self.visible = runner.visible;
        }
        if let Some(started) = self.started_at {
            let stable =
                now.duration_since(started).as_millis() as u64 >= settings.restart_reset_ms;
            if stable && self.restarts > 0 {
                self.restarts = 0;
            }
        }
    }

    fn view(&self) -> AppView {
        let suffix = match &self.status {
            Status::Backoff { retry_at } => {
                let left = retry_at.saturating_duration_since(Instant::now());
                let seconds = (left.as_millis() as u64).div_ceil(1000).max(1);
                format!(" ({} restarts, next in {seconds}s)", self.restarts)
            }
            Status::GivenUp => format!(" — gave up after {} crashes", self.restarts),
            Status::Stopped => " — stopped".to_string(),
            Status::Running if self.restarts > 0 => format!(" ({} restarts)", self.restarts),
            Status::Running => String::new(),
        };
        AppView {
            id: self.id,
            label: format!("{}{suffix}", self.cfg.name),
            running: matches!(self.status, Status::Running),
            visible: self.visible,
        }
    }
}

fn backoff(attempt: u32, settings: &Settings) -> Duration {
    let shift = attempt.saturating_sub(1).min(20);
    let ms = settings
        .restart_gap_ms
        .saturating_mul(1u64 << shift)
        .min(settings.restart_backoff_max_ms);
    Duration::from_millis(ms)
}

struct Supervisor {
    path: PathBuf,
    settings: Settings,
    instances: Vec<Instance>,
    next_id: u64,
    autostart: bool,
}

impl Supervisor {
    fn new(path: PathBuf, cfg: Config) -> Self {
        let mut sup = Supervisor {
            path,
            settings: cfg.settings,
            instances: Vec::new(),
            next_id: 1,
            autostart: startup::is_installed(),
        };
        for app in cfg.apps {
            let instance = Instance::new(sup.next_id, app);
            sup.next_id += 1;
            sup.instances.push(instance);
        }
        sup
    }

    fn start_all(&mut self) {
        let now = Instant::now();
        for instance in &mut self.instances {
            instance.start_or_fail(&self.settings, now);
        }
    }

    fn find(&mut self, id: u64) -> Option<&mut Instance> {
        self.instances.iter_mut().find(|i| i.id == id)
    }

    fn tick_all(&mut self) {
        let now = Instant::now();
        for instance in &mut self.instances {
            instance.tick(&self.settings, now);
        }
    }

    fn views(&self) -> Vec<AppView> {
        self.instances.iter().map(Instance::view).collect()
    }

    fn restore(&mut self, id: u64) {
        let settings = self.settings.clone();
        if let Some(instance) = self.find(id) {
            instance.restarts = 0;
            instance.start_or_fail(&settings, Instant::now());
        }
    }

    fn reload(&mut self) {
        let cfg = match config::load(&self.path) {
            Ok(cfg) => cfg,
            Err(err) => {
                tray::dlog(&format!("reload failed: {err}"));
                cli::message_box(
                    &format!("{}, cannot reload:\n{err}", self.path.display()),
                    cli::MB_ICONWARNING,
                );
                return;
            }
        };
        self.settings = cfg.settings;
        let mut next: Vec<Instance> = Vec::with_capacity(cfg.apps.len());
        for app in cfg.apps {
            let found = self.instances.iter().position(|i| i.cfg.name == app.name);
            match found {
                Some(index) => {
                    let mut instance = self.instances.remove(index);
                    if instance.cfg == app {
                        next.push(instance);
                    } else {
                        instance.stop();
                        instance.cfg = app;
                        instance.restarts = 0;
                        instance.start_or_fail(&self.settings, Instant::now());
                        next.push(instance);
                    }
                }
                None => {
                    let mut instance = Instance::new(self.next_id, app);
                    self.next_id += 1;
                    instance.start_or_fail(&self.settings, Instant::now());
                    next.push(instance);
                }
            }
        }
        for removed in self.instances.drain(..) {
            tray::dlog(&format!("reload: dropping {}", removed.cfg.name));
            let mut removed = removed;
            removed.stop();
        }
        self.instances = next;
    }

    fn toggle_autostart(&mut self) {
        let result = if self.autostart {
            startup::uninstall()
        } else {
            std::env::current_exe()
                .map_err(|e| format!("cannot locate tray-run.exe: {e}"))
                .and_then(|exe| startup::install(&exe, &self.path))
        };
        match result {
            Ok(()) => {
                self.autostart = startup::is_installed();
                tray::dlog(&format!("autostart installed={}", self.autostart));
            }
            Err(err) => {
                tray::dlog(&format!("autostart failed: {err}"));
                cli::message_box(&format!("Autostart failed:\n{err}"), cli::MB_ICONWARNING);
            }
        }
    }

    fn quit_all(&mut self) {
        for instance in &mut self.instances {
            instance.stop();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--install-startup") => return install_startup(),
        Some("--uninstall-startup") => return uninstall_startup(),
        Some("--help" | "-h" | "-?" | "/?") => return cli::emit(cli::TEXT, cli::MB_ICONINFO),
        Some("--version" | "-V") => {
            return cli::emit(
                &format!("tray-run {}", env!("CARGO_PKG_VERSION")),
                cli::MB_ICONINFO,
            )
        }
        _ => {}
    }
    let explicit = match parse_config(&args) {
        Ok(path) => path,
        Err(message) => usage_error(&message),
    };
    ensure_single_instance();
    let path = config::resolve_path(explicit);
    let cfg = match config::load(&path) {
        Ok(cfg) => cfg,
        Err(err) => {
            let hint = if path.exists() {
                "Fix the file and start tray-run again."
            } else {
                "Create the file and start tray-run again."
            };
            fatal(&format!("{}\n\n{err}\n\n{hint}", path.display()))
        }
    };

    let mut sup = Supervisor::new(path, cfg);
    let tray = tray::Tray::new();
    sup.start_all();

    let mut quit = false;
    loop {
        while let Some(action) = tray.poll() {
            match action {
                Action::Toggle(id) => {
                    if let Some(instance) = sup.find(id) {
                        if let Some(runner) = instance.runner.as_mut() {
                            runner.toggle();
                            instance.visible = runner.visible;
                        }
                    }
                }
                Action::Restart(id) => sup.restore(id),
                Action::Stop(id) => {
                    if let Some(instance) = sup.find(id) {
                        instance.stop();
                        instance.restarts = 0;
                    }
                }
                Action::Start(id) => sup.restore(id),
                Action::Reload => sup.reload(),
                Action::ToggleAutostart => sup.toggle_autostart(),
                Action::QuitAll => quit = true,
            }
        }
        sup.tick_all();
        tray.sync(sup.views(), sup.autostart);
        if quit {
            break;
        }
        std::thread::sleep(TICK);
    }
    sup.quit_all();
    tray::dlog("tray-run: exiting");
}

fn parse_config(args: &[String]) -> Result<Option<PathBuf>, String> {
    match args.first().map(String::as_str) {
        None => Ok(None),
        Some("--config") => match args.get(1) {
            Some(path) if args.len() == 2 => Ok(Some(PathBuf::from(path))),
            Some(_) => Err(format!("unexpected argument: {}\n\n{}", args[2], usage())),
            None => Err(format!("--config needs a path\n\n{}", usage())),
        },
        Some(flag) => Err(format!("unknown option: {flag}\n\n{}", usage())),
    }
}

fn ensure_single_instance() {
    let name: Vec<u16> = "tray-run-supervisor\0".encode_utf16().collect();
    let handle = unsafe {
        windows_sys::Win32::System::Threading::CreateMutexW(std::ptr::null(), 1, name.as_ptr())
    };
    if handle.is_null() {
        return;
    }
    let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    if error == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS {
        fatal("tray-run is already running.\nUse its tray menu.");
    }
}

fn install_startup() {
    let path = config::resolve_path(None);
    if !path.exists() {
        fatal(&format!(
            "config not found: {}\n\nCreate it first.",
            path.display()
        ));
    }
    let result = std::env::current_exe()
        .map_err(|e| format!("cannot locate tray-run.exe: {e}"))
        .and_then(|exe| startup::install(&exe, &path));
    match result {
        Ok(()) => cli::message_box(
            &format!(
                "Autostart installed for tray-run.\nConfig: {}",
                path.display()
            ),
            cli::MB_ICONINFO,
        ),
        Err(err) => fatal(&err),
    }
}

fn uninstall_startup() {
    match startup::uninstall() {
        Ok(()) => cli::message_box("Autostart removed.", cli::MB_ICONINFO),
        Err(err) => fatal(&err),
    }
}

fn usage() -> String {
    "usage:\n  tray-run [--config PATH]\n  tray-run --install-startup | --uninstall-startup\n  tray-run --help | --version\n\nRun tray-run --help for the full manual.".to_string()
}

fn fatal(message: &str) -> ! {
    cli::emit(&format!("tray-run: {message}"), cli::MB_ICONERROR);
    std::process::exit(1);
}

fn usage_error(message: &str) -> ! {
    cli::emit(&format!("tray-run: {message}"), cli::MB_ICONERROR);
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn parse_err(items: &[&str]) -> String {
        match parse_config(&args(items)) {
            Err(err) => err,
            Ok(_) => panic!("expected an error for {items:?}"),
        }
    }

    #[test]
    fn defaults_to_the_lookup_order() {
        assert_eq!(parse_config(&args(&[])), Ok(None));
    }

    #[test]
    fn config_takes_a_path() {
        assert_eq!(
            parse_config(&args(&["--config", "C:\\cfg\\tray-run.toml"])),
            Ok(Some(PathBuf::from("C:\\cfg\\tray-run.toml")))
        );
    }

    #[test]
    fn rejects_unknown_option() {
        let err = parse_err(&["--bogus"]);
        assert!(err.contains("unknown option: --bogus"), "{err}");
        assert!(err.contains("usage:"), "{err}");
    }

    #[test]
    fn config_needs_a_value() {
        let err = parse_err(&["--config"]);
        assert!(err.contains("--config needs a path"), "{err}");
    }

    #[test]
    fn config_takes_one_path() {
        let err = parse_err(&["--config", "a.toml", "extra"]);
        assert!(err.contains("unexpected argument: extra"), "{err}");
    }
}
