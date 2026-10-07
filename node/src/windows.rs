//! lyra-node on Windows: a service ("lyra node", automatic, LocalSystem)
//! installed by `lyra-node.exe install --url … --name …` (as administrator),
//! which copies the program to `C:\Program Files\lyra`, pairs, and starts it.
//! The service restarts itself after an update (its recovery setting);
//! uninstalling removes the service, its settings and the program.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceFailureActions,
    ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE: &str = "lyra-node";

fn quiet(mut c: Command) -> Command {
    lyra_system::shell::own_group(&mut c);
    c
}

/// Running with administrator rights (`net session` only works then).
pub fn is_admin() -> bool {
    let mut c = quiet(Command::new("net"));
    c.arg("session").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    c.status().is_ok_and(|s| s.success())
}

/// "Microsoft Windows 11 Pro 10.0.26100".
pub fn os_name() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let mut c = quiet(Command::new("powershell"));
        c.args(["-NoProfile", "-NonInteractive", "-Command", "$o = Get-CimInstance Win32_OperatingSystem; $o.Caption + ' ' + $o.Version"]);
        c.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Windows".into())
    })
    .clone()
}

/// Only SYSTEM and Administrators may read the file (it holds the token).
pub fn lock_down(path: &Path) {
    let mut c = quiet(Command::new("icacls"));
    c.arg(path).args(["/inheritance:r", "/grant:r", "*S-1-5-18:F", "*S-1-5-32-544:F"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let _ = c.status();
}

/// Where the program lives once installed.
pub fn program_dir() -> PathBuf {
    std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Program Files")).join("lyra")
}

/// `lyra-node.exe install --url … [--name …]`: copy, pair, create and start the service.
pub fn install(url: &str, name: &str) -> Result<String, String> {
    if !is_admin() {
        return Err("run this as administrator (right-click PowerShell → Run as administrator)".into());
    }
    let mut notes = Vec::new();
    // The program, where the service runs it from.
    let dir = program_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let target = dir.join("lyra-node.exe");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    // An earlier install: stop it first so its program can be replaced.
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE).map_err(|e| format!("service manager: {e}"))?;
    if let Ok(old) = manager.open_service(SERVICE, ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS) {
        let _ = old.stop();
        for _ in 0..20 {
            if old.query_status().is_ok_and(|s| s.current_state == ServiceState::Stopped) {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = old.delete();
        drop(old);
        std::thread::sleep(Duration::from_secs(1));
        notes.push("replaced the earlier install".to_string());
    }
    if me != target {
        std::fs::copy(&me, &target).map_err(|e| format!("couldn't copy to {}: {e}", target.display()))?;
    }
    // Pairing (headless: approve it in lyra), unless it's paired already.
    let config = crate::config_path();
    if !config.exists() {
        let token = crate::request_pairing(url, name, "node")?;
        let c = crate::NodeConfig { url: url.trim_end_matches('/').to_string(), token, name: name.to_string(), system: lyra_system::Settings::default() };
        let text = toml::to_string_pretty(&c).map_err(|e| e.to_string())?;
        crate::write_private(&config, &text)?;
        notes.push(format!("paired as {name}; settings in {}", config.display()));
    } else {
        notes.push(format!("already paired ({})", config.display()));
    }
    let info = ServiceInfo {
        name: OsString::from(SERVICE),
        display_name: OsString::from("lyra node"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: target.clone(),
        launch_arguments: vec![OsString::from("service-run")],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let service = manager
        .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS)
        .map_err(|e| format!("couldn't create the service: {e}"))?;
    let _ = service.set_description("Lets lyra work on this machine (checked against this machine's own rules).");
    // After an update the program exits; this starts the new one.
    let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(3) };
    let _ = service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86400)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(30) }]),
    });
    let _ = service.set_failure_actions_on_non_crash_failures(true);
    service.start(&[] as &[&OsStr]).map_err(|e| format!("couldn't start the service: {e}"))?;
    notes.push(format!("service \"lyra node\" installed and started ({})", target.display()));
    Ok(notes.join("\n"))
}

/// Remove the service (it stops when this program exits), its settings and
/// the program (deleted a moment after it exits).
pub fn uninstall() -> Vec<String> {
    let mut removed = Vec::new();
    if let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        && let Ok(s) = manager.open_service(SERVICE, ServiceAccess::DELETE)
        && s.delete().is_ok()
    {
        removed.push("the lyra node service".to_string());
    }
    let config = crate::config_path();
    if std::fs::remove_file(&config).is_ok() {
        removed.push(config.display().to_string());
    }
    let dir = program_dir();
    if dir.exists() {
        // A running program can't delete itself: a detached cmd does it once it's gone.
        use std::os::windows::process::CommandExt;
        const DETACHED: u32 = 0x0000_0008;
        const NO_WINDOW: u32 = 0x0800_0000;
        let script = format!("ping -n 4 127.0.0.1 > nul & rmdir /s /q \"{}\"", dir.display());
        if Command::new("cmd").args(["/c", &script]).creation_flags(DETACHED | NO_WINDOW).spawn().is_ok() {
            removed.push(dir.display().to_string());
        }
    }
    removed
}

static STATUS: OnceLock<ServiceStatusHandle> = OnceLock::new();

fn report(state: ServiceState, code: u32) {
    if let Some(h) = STATUS.get() {
        let _ = h.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running { ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN } else { ServiceControlAccept::empty() },
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        });
    }
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_args: Vec<OsString>) {
    let handler = |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            report(ServiceState::Stopped, 0);
            std::process::exit(0);
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(handle) = service_control_handler::register(SERVICE, handler) else { return };
    let _ = STATUS.set(handle);
    report(ServiceState::Running, 0);
    // Leftovers from an update.
    if let Ok(me) = std::env::current_exe() {
        let _ = std::fs::remove_file(me.with_extension("old"));
    }
    crate::run();
}

/// `lyra-node.exe service-run`: what Windows starts.
pub fn run_service() -> Result<(), String> {
    service_dispatcher::start(SERVICE, ffi_service_main).map_err(|e| format!("this is for Windows to start (as the lyra node service): {e}"))
}
