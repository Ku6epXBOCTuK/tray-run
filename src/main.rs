#![windows_subsystem = "windows"]

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
    single_shot: bool,
}

impl Instance {
    fn new(id: u64, cfg: AppConfig, single_shot: bool) -> Self {
        Instance {
            id,
            cfg,
            status: Status::Stopped,
            visible: false,
            restarts: 0,
            started_at: None,
            runner: None,
            single_shot,
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
        if self.single_shot || console::is_user_stop(code) {
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
    config_mode: bool,
    autostart: bool,
}

impl Supervisor {
    fn new(path: PathBuf, cfg: Config, config_mode: bool) -> Self {
        let mut sup = Supervisor {
            path,
            settings: cfg.settings,
            instances: Vec::new(),
            next_id: 1,
            config_mode,
            autostart: startup::is_installed(),
        };
        for app in cfg.apps {
            let instance = Instance::new(sup.next_id, app, !config_mode);
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
                message_box(
                    &format!("{}, cannot reload:\n{err}", self.path.display()),
                    MB_ICONWARNING,
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
                    let mut instance = Instance::new(self.next_id, app, false);
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
                message_box(&format!("Autostart failed:\n{err}"), MB_ICONWARNING);
            }
        }
    }

    fn quit_all(&mut self) {
        for instance in &mut self.instances {
            instance.stop();
        }
    }

    fn finished_single_shot(&self) -> bool {
        if self.config_mode {
            return false;
        }
        self.instances
            .iter()
            .all(|i| matches!(i.status, Status::Stopped | Status::GivenUp))
    }
}

enum Mode {
    Config(Option<PathBuf>),
    AdHoc { name: String, command: String },
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--install-startup") => return install_startup(),
        Some("--uninstall-startup") => return uninstall_startup(),
        _ => {}
    }
    let (path, cfg, config_mode) = match parse_mode(&args) {
        Ok(Mode::Config(explicit)) => {
            ensure_single_instance();
            let path = config::resolve_path(explicit);
            let cfg = match config::load(&path) {
                Ok(cfg) => cfg,
                Err(err) => fatal(&format!(
                    "{}\n\n{err}\n\nCreate the file and start tray-run again.",
                    path.display()
                )),
            };
            (path, cfg, true)
        }
        Ok(Mode::AdHoc { name, command }) => (
            config::resolve_path(None),
            Config {
                apps: vec![AppConfig {
                    name,
                    command,
                    dir: None,
                    restart: Restart::Never,
                    start_hidden: true,
                }],
                settings: Settings {
                    restart_gap_ms: 2_000,
                    restart_backoff_max_ms: 60_000,
                    restart_max: 0,
                    restart_reset_ms: 60_000,
                },
            },
            false,
        ),
        Err(message) => fatal(&message),
    };

    let mut sup = Supervisor::new(path, cfg, config_mode);
    let tray = tray::Tray::new();
    if sup.config_mode {
        sup.start_all();
    } else if let Some(instance) = sup.instances.first_mut() {
        match instance.start() {
            Ok(()) => {}
            Err(StartError::Exited(_)) => return,
            Err(StartError::Failed(err)) => fatal(&err),
        }
    }

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
        tray.sync(sup.views(), sup.autostart, sup.config_mode);
        if quit || sup.finished_single_shot() {
            break;
        }
        std::thread::sleep(TICK);
    }
    sup.quit_all();
    tray::dlog("tray-run: exiting");
}

fn parse_mode(args: &[String]) -> Result<Mode, String> {
    match args.first().map(String::as_str) {
        None => Ok(Mode::Config(None)),
        Some("--config") => match args.get(1) {
            Some(path) => Ok(Mode::Config(Some(PathBuf::from(path)))),
            None => Err("--config needs a path".to_string()),
        },
        Some(_) => {
            let mut index = 0;
            let mut name = None;
            if args.first().map(String::as_str) == Some("--name") {
                name = args.get(1).cloned();
                index = 2;
            }
            let rest = &args[index..];
            let rest = if rest.first().map(String::as_str) == Some("--") {
                &rest[1..]
            } else {
                rest
            };
            if rest.is_empty() {
                return Err(usage());
            }
            let name = name.unwrap_or_else(|| {
                let program = &rest[0];
                std::path::Path::new(program)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| program.clone())
            });
            Ok(Mode::AdHoc {
                name,
                command: console::join_args(rest),
            })
        }
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
        Ok(()) => message_box(
            &format!(
                "Autostart installed for tray-run.\nConfig: {}",
                path.display()
            ),
            MB_ICONINFO,
        ),
        Err(err) => fatal(&err),
    }
}

fn uninstall_startup() {
    match startup::uninstall() {
        Ok(()) => message_box("Autostart removed.", MB_ICONINFO),
        Err(err) => fatal(&err),
    }
}

const MB_ICONINFO: u32 = 0x40;
const MB_ICONWARNING: u32 = 0x30;
const MB_ICONERROR: u32 = 0x10;

fn usage() -> String {
    "usage:\n  tray-run [--config PATH]\n  tray-run --install-startup | --uninstall-startup\n  tray-run [--name NAME] [--] <command> [args...]".to_string()
}

fn message_box(text: &str, icon: u32) {
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

fn fatal(message: &str) -> ! {
    message_box(message, MB_ICONERROR);
    std::process::exit(1);
}
