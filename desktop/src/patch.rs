use crate::task::{Result, Task};
use betterwx::{feature, install, store::Store};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Weixin {
    pub path: String,
    pub version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub path: String,
    pub version: String,
    pub enabled: bool,
    pub supported: bool,
    pub issue: Option<String>,
    pub outdated: bool,
    pub backup: bool,
    pub backup_path: String,
}

pub fn discover() -> Result<Vec<Weixin>> {
    install::scan()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| {
            Ok(Weixin {
                version: install::version(&p).map_err(|e| e.to_string())?,
                path: betterwx::store::display(&p),
            })
        })
        .collect()
}

fn target(path: &str) -> Result<PathBuf> {
    let path = install::resolve(PathBuf::from(path)).map_err(|e| e.to_string())?;
    if !path
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("Weixin.dll"))
    {
        return Err("请选择 Weixin.exe 或 Weixin.dll".into());
    }
    path.canonicalize().map_err(|e| e.to_string())
}

fn inspect(path: &str, task: &Task) -> Result<(Store, Vec<feature::Prepared>)> {
    let path = target(path)?;
    task.record("target", &betterwx::store::display(&path));
    let mut store = Store::open(&path, &task.progress).map_err(|e| e.to_string())?;
    let prepared = feature::scan(&mut store, &task.progress).map_err(|e| e.to_string())?;
    feature::inspect(&mut store, &prepared, &task.progress).map_err(|e| e.to_string())?;
    Ok((store, prepared))
}

fn status(store: &Store, prepared: &[feature::Prepared]) -> Result<Status> {
    let patch = prepared
        .iter()
        .find(|p| p.feature.id == "revoke")
        .ok_or("找不到防撤回分析结果")?;
    let file = &store.files[&store.target];
    let issue = patch
        .issue(store)
        .cloned()
        .or_else(|| file.source_issue.clone());
    Ok(Status {
        path: betterwx::store::display(&store.target),
        version: install::version(&store.target).map_err(|e| e.to_string())?,
        enabled: patch.state(store).enabled(),
        supported: issue.is_none(),
        issue,
        outdated: patch.outdated(store),
        backup: Store::backup_exists(&store.target).map_err(|e| e.to_string())?,
        backup_path: Store::backup_path(&store.target)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned(),
    })
}

pub fn scan(path: &str, task: &Arc<Task>) -> Result<Status> {
    let (store, prepared) = inspect(path, task)?;
    status(&store, &prepared)
}

pub fn change(path: &str, enabled: bool, task: &Arc<Task>) -> Result<Status> {
    let (mut store, prepared) = inspect(path, task)?;
    let before = status(&store, &prepared)?;
    if let Some(issue) = before.issue {
        return Err(issue);
    }
    if before.enabled && enabled && before.outdated {
        return Err("当前防撤回补丁需要重新应用，请先关闭再开启".into());
    }
    let patch = prepared.iter().find(|p| p.feature.id == "revoke").unwrap();
    if before.enabled == enabled && !before.outdated {
        return Ok(before);
    }
    let contents = feature::change(
        &store,
        patch,
        enabled.then_some(0),
        &prepared,
        &task.progress,
    )
    .map_err(|e| e.to_string())?;
    let targets: Vec<_> = contents.keys().cloned().collect();
    #[cfg(windows)]
    if !betterwx::process::using(&targets)
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        return Err("微信正在使用程序文件，请完全退出微信后重试".into());
    }
    if let Some(e) = store.write_error(&targets) {
        return Err(format!(
            "文件不可写，请退出微信，或以管理员身份重新打开。{e}"
        ));
    }
    task.progress.check().map_err(|e| e.to_string())?;
    task.cancellable.store(false, Ordering::Relaxed);
    task.record(
        "write-start",
        if enabled {
            "enable revoke"
        } else {
            "disable revoke"
        },
    );
    store
        .save(contents, &task.progress)
        .map_err(|e| e.to_string())?;
    let (after, prepared) = inspect(path, task)?;
    let result = status(&after, &prepared)?;
    if result.enabled != enabled || !result.supported {
        return Err("写入后检查未通过，请复制诊断信息".into());
    }
    task.record(
        "write-verified",
        if enabled { "enabled" } else { "disabled" },
    );
    Ok(result)
}

pub fn pick() -> Result<Option<String>> {
    install::pick()
        .map(|p| p.map(|p| betterwx::store::display(Path::new(&p))))
        .map_err(|e| e.to_string())
}
