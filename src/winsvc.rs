//! Conductor as a Windows service: `conductor.exe [--config FILE] service install|uninstall|start|stop`.
//! The service runs `conductor.exe [--config FILE] service run` as LocalSystem, starts with Windows, and writes its
//! log to `<data_dir>\conductor.log`. When it fails it says so (a service-specific exit code, not 0), and Windows
//! starts it again: 10 s later, then 30 s, then every minute (the count starts over after a day without a failure).

use std::{ffi::OsString, path::PathBuf, sync::Mutex, sync::OnceLock, time::Duration};

use anyhow::{Context, Result, bail};
use windows_service::{
    define_windows_service,
    service::{
        Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl,
        ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
        ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};

const NAME: &str = "Conductor";
/// The service-specific exit code of a service that failed (any error: settings it can't read, a port in use…).
const FAILED: u32 = 1;
static CONFIG: OnceLock<Option<PathBuf>> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

pub fn command(args: &[String], config: Option<PathBuf>) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("install") => install(config),
        Some("uninstall") => uninstall(),
        Some("start") => {
            let svc = open(ServiceAccess::START | ServiceAccess::QUERY_STATUS)?;
            svc.start::<&str>(&[])?;
            if !wait_for(&svc, ServiceState::Running, 30) {
                bail!("Conductor didn't start within 30 s; see conductor.log in its data folder");
            }
            println!("Conductor started");
            Ok(())
        }
        Some("stop") => {
            open(ServiceAccess::STOP)?.stop()?;
            println!("Conductor stopping");
            Ok(())
        }
        Some("run") => {
            let _ = CONFIG.set(config);
            service_dispatcher::start(NAME, ffi_service_main).context("this is started by Windows (use `service start`)")?;
            Ok(())
        }
        _ => bail!("conductor service install|uninstall|start|stop"),
    }
}

fn open(access: ServiceAccess) -> Result<windows_service::service::Service> {
    let mgr = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    mgr.open_service(NAME, access).context("the Conductor service isn't installed (conductor service install, as administrator)")
}

fn install(config: Option<PathBuf>) -> Result<()> {
    let mgr = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .context("installing a service needs an administrator prompt")?;
    let mut launch: Vec<OsString> = vec![];
    if let Some(c) = config {
        launch.push("--config".into());
        launch.push(std::fs::canonicalize(&c).with_context(|| format!("{}", c.display()))?.into_os_string());
    }
    launch.push("service".into());
    launch.push("run".into());
    let info = ServiceInfo {
        name: NAME.into(),
        display_name: "Conductor music server".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe()?,
        launch_arguments: launch,
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    // (START too: Windows asks for it to set restarting as what's done when it fails)
    let svc = mgr.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?;
    svc.set_description("Music server for Encore and any Subsonic app")?;
    if let Err(e) = restart_on_failure(&svc) {
        eprintln!("(Windows won't start Conductor again by itself if it fails: {e:#})");
    }
    println!("Conductor service installed; it starts with Windows. Start it now: conductor service start");
    Ok(())
}

/// What Windows does when the service fails: start it again after 10 s, 30 s, then every minute; the count starts over
/// after a day without a failure. Also when it stops with an error of its own (FAILED), not only when it crashes.
/// (install.ps1 sets the same with sc.exe on a service installed by an older version.)
fn restart_on_failure(svc: &Service) -> Result<()> {
    let restart = |secs| ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(secs) };
    svc.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart(10), restart(30), restart(60)]),
    })?;
    svc.set_failure_actions_on_non_crash_failures(true)?;
    Ok(())
}

/// Wait up to `secs` for the service to reach `state`.
fn wait_for(svc: &windows_service::service::Service, state: ServiceState, secs: u64) -> bool {
    for _ in 0..secs * 4 {
        match svc.query_status() {
            Ok(s) if s.current_state == state => return true,
            Ok(s) if state == ServiceState::Running && s.current_state == ServiceState::Stopped => return false, // it gave up
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    false
}

fn uninstall() -> Result<()> {
    let svc = open(ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE)?;
    if svc.query_status()?.current_state != ServiceState::Stopped {
        let _ = svc.stop();
        if !wait_for(&svc, ServiceState::Stopped, 30) {
            eprintln!("Conductor is taking long to stop; it's removed once it has");
        }
    }
    svc.delete()?;
    println!("Conductor service removed (its data stays)");
    Ok(())
}

/// The service's state for Windows; `exit` is 0, or FAILED when it stopped because something went wrong.
fn status(state: ServiceState, exit: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        // a failure as a service-specific code (dwWin32ExitCode ERROR_SERVICE_SPECIFIC_ERROR): before, every stop
        // said 0, a clean stop, and Windows never started a failed service again
        exit_code: if exit == 0 { ServiceExitCode::Win32(0) } else { ServiceExitCode::ServiceSpecific(exit) },
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    }
}

fn service_main(_args: Vec<OsString>) {
    let config = CONFIG.get().cloned().flatten();
    // the log goes next to the data (a service has no console)
    let log_dir = match conductor::config::Config::load(config.clone()) {
        Ok(cfg) => {
            conductor::config::move_old_names(&cfg); // before the log file makes the new folder
            cfg.data_dir
        }
        // settings that can't be read: the log goes beside them, so why the service didn't start is written where
        // install.ps1 says to look (before, nothing was written anywhere)
        Err(_) => config.as_deref().and_then(|c| c.parent()).map(PathBuf::from).unwrap_or_else(conductor::config::default_data_dir),
    };
    let _ = std::fs::create_dir_all(&log_dir);
    crate::init_logs(std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("conductor.log")).ok());
    // (Conductor's own errors are written by run_service; these are from talking to Windows, and the process ending
    // without saying it stopped counts as a failure too)
    if let Err(e) = run_service(config) {
        tracing::error!("Conductor service: {e:#}");
    }
}

fn run_service(config: Option<PathBuf>) -> Result<()> {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx = Mutex::new(Some(tx));
    let handler = move |ev| match ev {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            if let Some(t) = tx.lock().unwrap().take() {
                let _ = t.send(());
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let handle = service_control_handler::register(NAME, handler)?;
    handle.set_service_status(status(ServiceState::Running, 0))?;
    let rt = tokio::runtime::Runtime::new()?;
    // stopped as from a terminal (Ctrl+C, SIGTERM): requests under way finish, the workers stop, the database closes
    let res = rt.block_on(crate::run(
        vec![],
        config,
        Box::pin(async move {
            let _ = rx.await;
            let _ = handle.set_service_status(status(ServiceState::StopPending, 0));
        }),
    ));
    rt.shutdown_timeout(Duration::from_secs(1));
    // written before Windows is told it stopped (which may end this process)
    if let Err(e) = &res {
        tracing::error!("Conductor service: {e:#}");
    }
    handle.set_service_status(status(ServiceState::Stopped, if res.is_ok() { 0 } else { FAILED }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_reported_as_one() {
        // Windows' recovery actions (restart) apply only to a service that says it failed
        assert_eq!(status(ServiceState::Stopped, FAILED).exit_code, ServiceExitCode::ServiceSpecific(1));
        assert_eq!(status(ServiceState::Stopped, 0).exit_code, ServiceExitCode::Win32(0));
        assert_eq!(status(ServiceState::Running, 0).controls_accepted, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN);
    }
}
