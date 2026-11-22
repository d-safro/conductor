//! Maestro as a Windows service: `maestro.exe [--config FILE] service install|uninstall|start|stop`.
//! The service runs `maestro.exe [--config FILE] service run` as LocalSystem, starts with Windows, and writes its
//! log to `<data_dir>\maestro.log`.

use std::{ffi::OsString, path::PathBuf, sync::Mutex, sync::OnceLock, time::Duration};

use anyhow::{Context, Result, bail};
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo, ServiceStartType,
        ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};

const NAME: &str = "Maestro";
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
                bail!("Maestro didn't start within 30 s; see maestro.log in its data folder");
            }
            println!("Maestro started");
            Ok(())
        }
        Some("stop") => {
            open(ServiceAccess::STOP)?.stop()?;
            println!("Maestro stopping");
            Ok(())
        }
        Some("run") => {
            let _ = CONFIG.set(config);
            service_dispatcher::start(NAME, ffi_service_main).context("this is started by Windows (use `service start`)")?;
            Ok(())
        }
        _ => bail!("maestro service install|uninstall|start|stop"),
    }
}

fn open(access: ServiceAccess) -> Result<windows_service::service::Service> {
    let mgr = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    mgr.open_service(NAME, access).context("the Maestro service isn't installed (maestro service install, as administrator)")
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
        display_name: "Maestro music server".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe()?,
        launch_arguments: launch,
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let svc = mgr.create_service(&info, ServiceAccess::CHANGE_CONFIG)?;
    svc.set_description("Music server for Encore and any Subsonic app")?;
    println!("Maestro service installed; it starts with Windows. Start it now: maestro service start");
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
            eprintln!("Maestro is taking long to stop; it's removed once it has");
        }
    }
    svc.delete()?;
    println!("Maestro service removed (its data stays)");
    Ok(())
}

fn status(state: ServiceState) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    }
}

fn service_main(_args: Vec<OsString>) {
    let config = CONFIG.get().cloned().flatten();
    // the log goes next to the data (a service has no console)
    if let Ok(cfg) = maestro::config::Config::load(config.clone()) {
        let _ = std::fs::create_dir_all(&cfg.data_dir);
        crate::init_logs(std::fs::OpenOptions::new().create(true).append(true).open(cfg.data_dir.join("maestro.log")).ok());
    }
    if let Err(e) = run_service(config) {
        tracing::error!("Maestro service: {e:#}");
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
    handle.set_service_status(status(ServiceState::Running))?;
    let rt = tokio::runtime::Runtime::new()?;
    let res = rt.block_on(crate::run(
        vec![],
        config,
        Box::pin(async move {
            let _ = rx.await;
        }),
    ));
    handle.set_service_status(status(ServiceState::Stopped))?;
    res
}
