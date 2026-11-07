use crate::{
    patch,
    task::{Result, Snapshot, Task},
    updates::{self, Signed},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tauri::{Emitter, Manager, State};

pub struct AppState {
    pub task: Mutex<Option<Arc<Task>>>,
    pub busy: Arc<AtomicBool>,
    pub logs: PathBuf,
    pub update: Mutex<Option<Signed>>,
    pub package: Mutex<Option<PathBuf>>,
}

fn begin<F>(app: tauri::AppHandle, state: &AppState, action: &str, work: F) -> Result<String>
where
    F: FnOnce(Arc<Task>, tauri::AppHandle) -> Result<Value> + Send + 'static,
{
    if state
        .busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("已有任务正在进行，请等待完成".into());
    }
    let task = match Task::new(action, &state.logs) {
        Ok(t) => t,
        Err(e) => {
            state.busy.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    let id = task.id.clone();
    *state.task.lock().unwrap() = Some(task.clone());
    let busy = state.busy.clone();
    let events = app.clone();
    let observer = task.clone();
    thread::spawn(move || {
        loop {
            let s = observer.snapshot();
            let running = s.status == "running";
            let _ = events.emit("operation", s);
            if !running {
                break;
            }
            thread::sleep(Duration::from_millis(250));
        }
    });
    thread::spawn(move || {
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(task.clone(), app)))
                .unwrap_or_else(|_| Err("任务异常中止，请复制诊断信息".into()));
        task.finish(outcome);
        busy.store(false, Ordering::SeqCst);
    });
    Ok(id)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    version: &'static str,
    repository: &'static str,
    runtime: crate::runtime::Runtime,
    locations: Vec<patch::Weixin>,
    update_configured: bool,
}

#[tauri::command]
fn info() -> Result<Info> {
    Ok(Info {
        version: env!("CARGO_PKG_VERSION"),
        repository: updates::REPOSITORY,
        runtime: crate::runtime::status(),
        locations: patch::discover()?,
        update_configured: !updates::PUBLIC_KEY.trim().is_empty(),
    })
}

#[tauri::command]
fn pick_weixin(state: State<AppState>) -> Result<Option<String>> {
    if state.busy.load(Ordering::SeqCst) {
        return Err("请等待当前任务完成".into());
    }
    patch::pick()
}

#[tauri::command]
fn scan(app: tauri::AppHandle, state: State<AppState>, path: String) -> Result<String> {
    begin(app, &state, "检查微信", move |task, _| {
        Ok(json!({"kind":"scan","status":patch::scan(&path,&task)?}))
    })
}

#[tauri::command]
fn set_revoke(
    app: tauri::AppHandle,
    state: State<AppState>,
    path: String,
    enabled: bool,
) -> Result<String> {
    begin(
        app,
        &state,
        if enabled {
            "开启防撤回"
        } else {
            "关闭防撤回"
        },
        move |task, _| Ok(json!({"kind":"scan","status":patch::change(&path,enabled,&task)?})),
    )
}

#[tauri::command]
fn check_update(app: tauri::AppHandle, state: State<AppState>) -> Result<String> {
    begin(app, &state, "检查应用更新", |task, app| {
        let update = updates::check(&task)?;
        let value = update.as_ref().map(|s| s.manifest.clone());
        *app.state::<AppState>().update.lock().unwrap() = update;
        *app.state::<AppState>().package.lock().unwrap() = None;
        Ok(json!({"kind":"update","update":value}))
    })
}

#[tauri::command]
fn download_update(app: tauri::AppHandle, state: State<AppState>) -> Result<String> {
    let signed = state.update.lock().unwrap().clone().ok_or("请先检查更新")?;
    begin(app, &state, "下载应用更新", move |task, app| {
        let path = updates::fetch(&signed, &task)?;
        *app.state::<AppState>().package.lock().unwrap() = Some(path);
        Ok(json!({"kind":"downloaded","version":signed.manifest.version}))
    })
}

#[tauri::command]
fn install_update(app: tauri::AppHandle, state: State<AppState>) -> Result<String> {
    let signed = state.update.lock().unwrap().clone().ok_or("请先检查更新")?;
    let path = state
        .package
        .lock()
        .unwrap()
        .clone()
        .ok_or("请先下载更新")?;
    begin(app, &state, "安装应用更新", move |task, app| {
        updates::prepare(&signed, &path, &task)?;
        app.exit(0);
        Ok(json!({"kind":"restarting"}))
    })
}

#[tauri::command]
fn update_runtime(app: tauri::AppHandle, state: State<AppState>) -> Result<String> {
    begin(app, &state, "更新 WebView2", |task, _| {
        Ok(json!({"kind":"runtime","runtime":crate::runtime::install(&task)?}))
    })
}

#[tauri::command]
fn clear_downloads(state: State<AppState>) -> Result<()> {
    if state
        .busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("请先暂停下载".into());
    }
    let r = crate::download::clear_cache();
    *state.package.lock().unwrap() = None;
    state.busy.store(false, Ordering::SeqCst);
    r
}

#[tauri::command]
fn cancel(state: State<AppState>) -> Result<()> {
    if let Some(task) = state.task.lock().unwrap().as_ref() {
        if task.snapshot().status == "running" {
            if !task.cancellable.load(Ordering::SeqCst) {
                return Err("正在写入或安装，请等待完成".into());
            }
            task.progress.cancel();
        }
    }
    Ok(())
}

#[tauri::command]
fn current_task(state: State<AppState>) -> Option<Snapshot> {
    state.task.lock().unwrap().as_ref().map(|t| t.snapshot())
}

#[tauri::command]
fn elevate(app: tauri::AppHandle, state: State<AppState>) -> Result<()> {
    if state.busy.load(Ordering::SeqCst) {
        return Err("请等待当前任务完成".into());
    }
    crate::native::elevate()?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn open_repository() -> Result<()> {
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            crate::native::wide("open").as_ptr(),
            crate::native::wide("https://github.com/shusfun/BetterWX").as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize <= 32 {
        return Err("无法打开 GitHub 仓库".into());
    }
    Ok(())
}

#[tauri::command]
fn open_diagnostics(app: tauri::AppHandle, state: State<AppState>, data: Value) -> Result<()> {
    let text = data
        .get("diagnostics")
        .and_then(Value::as_str)
        .unwrap_or("暂无诊断信息")
        .to_owned();
    let _ = state;
    thread::spawn(move || {
        if let Err(error) = crate::native::diagnostics(text, app.clone()) {
            crate::native::message_title("诊断窗口无法打开", &error);
        }
    });
    Ok(())
}

#[tauri::command]
fn open_notice(title: String, message: String) -> Result<()> {
    thread::spawn(move || crate::native::message_title(&title, &message));
    Ok(())
}

#[tauri::command]
fn frontend_ready(app: tauri::AppHandle) -> Result<()> {
    if let Some(path) = std::env::args()
        .skip_while(|a| a != "--updated-ready")
        .nth(1)
    {
        let p = PathBuf::from(path);
        let parent = p.parent().ok_or("就绪路径无效")?;
        if parent.parent() != Some(std::env::temp_dir().as_path())
            || !parent
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("BetterWX-update-"))
            || p.file_name().and_then(|n| n.to_str()) != Some("ready")
        {
            return Err("就绪路径无效".into());
        }
        std::fs::write(p, env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    }
    app.get_webview_window("main")
        .ok_or("窗口尚未就绪")?
        .show()
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn run(session: PathBuf) -> Result<()> {
    let busy = Arc::new(AtomicBool::new(false));
    let window_busy = busy.clone();
    tauri::Builder::default()
        .manage(AppState {
            task: Mutex::new(None),
            busy,
            logs: session.join("logs"),
            update: Mutex::new(None),
            package: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            info,
            pick_weixin,
            scan,
            set_revoke,
            check_update,
            download_update,
            install_update,
            update_runtime,
            clear_downloads,
            cancel,
            current_task,
            elevate,
            open_repository,
            open_diagnostics,
            open_notice,
            frontend_ready
        ])
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("BetterWX")
            .inner_size(560.0, 240.0)
            .min_inner_size(560.0, 240.0)
            .data_directory(session.join("webview"))
            .visible(false)
            .build()?;
            Ok(())
        })
        .on_window_event(move |window, event| {
            if window.label() == "main"
                && let tauri::WindowEvent::CloseRequested { api, .. } = event
            {
                if window_busy.load(Ordering::SeqCst) {
                    api.prevent_close();
                    let _ = window.emit("close-blocked", "请先暂停下载，或等待当前操作完成");
                }
            }
        })
        .run(tauri::generate_context!())
        .map_err(|e| e.to_string())
}
