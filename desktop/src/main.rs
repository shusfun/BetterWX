#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use betterwx_desktop::{app, native, patch, task::Task, updates};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn clean_old_sessions() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if entry
            .file_type()
            .is_ok_and(|t| t.is_dir() && !t.is_symlink())
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("BetterWX-session-"))
            && p.join("owned").is_file()
        {
            if let Ok(lock) = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(p.join("lock"))
            {
                if lock.try_lock().is_ok() {
                    native::wait_until_unused_and_remove(&p);
                }
            }
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--internal-update") {
        return updates::run_helper(Path::new(args.get(2).ok_or("缺少更新请求")?));
    }
    if args
        .get(1)
        .is_some_and(|a| a == "--engine-scan" || a == "--engine-enable" || a == "--engine-disable")
    {
        let session = tempfile::tempdir().map_err(|e| e.to_string())?;
        let task = Task::new("engine", session.path())?;
        let p = args.get(2).ok_or("缺少微信路径")?;
        let r = if args[1] == "--engine-scan" {
            patch::scan(p, &task)
        } else {
            patch::change(p, args[1] == "--engine-enable", &task)
        };
        let value = r.map(|s| serde_json::to_value(s).unwrap());
        task.finish(value.clone());
        println!("{}", serde_json::to_string(&task.snapshot()).unwrap());
        return value.map(|_| ());
    }
    if args.get(1).is_some_and(|a| a == "--gui-session") {
        let session = PathBuf::from(args.get(2).ok_or("缺少运行目录")?);
        if session.parent() != Some(std::env::temp_dir().as_path())
            || !session
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("BetterWX-session-"))
            || !session.join("owned").is_file()
        {
            return Err("运行目录无效".into());
        }
        return app::run(session);
    }
    if let Some(i) = args.iter().position(|a| a == "--rollback-report") {
        if let Some(path) = args.get(i + 1) {
            if let Ok(message) = fs::read_to_string(path) {
                native::message(&format!("新版未能启动，已恢复旧版。\n{message}"));
            }
        }
    }
    let _instance = if args.iter().any(|a| a == "--wait-for-instance") {
        let mut instance = None;
        for _ in 0..100 {
            if let Ok(value) = native::single_instance() {
                instance = Some(value);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        instance.ok_or("旧窗口尚未退出，请关闭后重试")?
    } else {
        native::single_instance()?
    };
    clean_old_sessions();
    let session = tempfile::Builder::new()
        .prefix("BetterWX-session-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    fs::write(session.path().join("owned"), "BetterWX").map_err(|e| e.to_string())?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(session.path().join("lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    if !native::ensure_runtime(&session.path().join("logs"), false)? {
        return Ok(());
    }
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command.arg("--gui-session").arg(session.path());
    if let Some(i) = args.iter().position(|a| a == "--updated-ready") {
        if let Some(p) = args.get(i + 1) {
            command.arg("--updated-ready").arg(p);
        }
    }
    let status = command.status().map_err(|e| e.to_string())?;
    drop(lock);
    native::wait_until_unused_and_remove(session.path());
    if !status.success() {
        return Err("界面异常退出；微信文件不会因此自动修改".into());
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        native::message(&e);
        std::process::exit(1);
    }
}
