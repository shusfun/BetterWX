use crate::{
    download,
    task::{Result, Task},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

pub const REPOSITORY: &str = "shusfun/BetterWX";
pub const PUBLIC_KEY: &str = include_str!("../update-public-key.txt");
const MAX_METADATA: u64 = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: String,
    pub notes: String,
    pub target: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Signed {
    pub manifest: Manifest,
    pub document: String,
    pub signature: String,
}

fn decode_key(key: &str) -> Result<VerifyingKey> {
    let bytes = STANDARD
        .decode(key.trim())
        .map_err(|_| "更新公钥格式错误")?;
    VerifyingKey::from_bytes(&bytes.try_into().map_err(|_| "更新公钥长度错误")?)
        .map_err(|e| e.to_string())
}

pub fn verify(document: &[u8], signature: &str, key: &str, current: &str) -> Result<Manifest> {
    let sig = STANDARD
        .decode(signature.trim())
        .map_err(|_| "更新签名格式错误")?;
    decode_key(key)?
        .verify(
            document,
            &Signature::from_slice(&sig).map_err(|_| "更新签名长度错误")?,
        )
        .map_err(|_| "更新清单签名无效")?;
    let manifest: Manifest = serde_json::from_slice(document).map_err(|e| e.to_string())?;
    let version = semver::Version::parse(&manifest.version).map_err(|_| "版本号无效")?;
    if !version.pre.is_empty() {
        return Err("不接收预发布版本".into());
    }
    if version <= semver::Version::parse(current).map_err(|e| e.to_string())? {
        return Err("没有更高版本，已阻止降级或重复更新".into());
    }
    let url = reqwest::Url::parse(&manifest.url).map_err(|e| e.to_string())?;
    let prefix = format!("/{REPOSITORY}/releases/download/v{}/", manifest.version);
    if manifest.target != "windows-x86_64"
        || url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.path().starts_with(&prefix)
        || !url.path().ends_with("/BetterWX.exe")
        || url.query().is_some()
        || !url.username().is_empty()
        || manifest.size == 0
        || manifest.size > 512 * 1024 * 1024
        || manifest.sha256.len() != 64
        || hex::decode(&manifest.sha256).is_err()
    {
        return Err("更新清单的平台、地址或文件信息不合法".into());
    }
    Ok(manifest)
}

fn get_text(url: &str) -> Result<String> {
    let response = download::client()?
        .get(url)
        .send()
        .map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("当前仓库尚未发布可用的桌面更新".into());
    }
    let mut body = String::new();
    response
        .error_for_status()
        .map_err(|e| e.to_string())?
        .take(MAX_METADATA + 1)
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() > MAX_METADATA as usize {
        return Err("更新清单过大".into());
    }
    Ok(body)
}

pub fn check(task: &Arc<Task>) -> Result<Option<Signed>> {
    decode_key(PUBLIC_KEY)?;
    let url = format!("https://github.com/{REPOSITORY}/releases/latest/download/update.json");
    task.record("update-source", &url);
    let document = get_text(&url)?;
    let signature = get_text(&format!("{url}.sig"))?;
    // Verify even when the published version is not newer.
    let value: Manifest = serde_json::from_str(&document).map_err(|e| e.to_string())?;
    let older = semver::Version::parse(&value.version).map_err(|e| e.to_string())?
        <= semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let validation_current = if older {
        "0.0.0"
    } else {
        env!("CARGO_PKG_VERSION")
    };
    let manifest = verify(
        document.as_bytes(),
        &signature,
        PUBLIC_KEY,
        validation_current,
    )?;
    if older {
        task.record("update-result", "already latest");
        return Ok(None);
    }
    task.record("update-result", &manifest.version);
    Ok(Some(Signed {
        manifest,
        document,
        signature,
    }))
}

pub fn fetch(signed: &Signed, task: &Arc<Task>) -> Result<PathBuf> {
    let m = verify(
        signed.document.as_bytes(),
        &signed.signature,
        PUBLIC_KEY,
        env!("CARGO_PKG_VERSION"),
    )?;
    download::download(&m.url, Some((m.size, &m.sha256)), task)
}

#[derive(Serialize, Deserialize)]
struct InstallRequest {
    target: PathBuf,
    package: PathBuf,
    parent: u32,
    document: String,
    signature: String,
    ready: PathBuf,
}

pub fn prepare(signed: &Signed, package: &Path, task: &Arc<Task>) -> Result<PathBuf> {
    let manifest = verify(
        signed.document.as_bytes(),
        &signed.signature,
        PUBLIC_KEY,
        env!("CARGO_PKG_VERSION"),
    )?;
    if download::sha256(package)? != manifest.sha256 {
        return Err("更新包已变化，请重新下载".into());
    }
    let target = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let parent = target.parent().ok_or("程序目录无效")?;
    let probe = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "程序目录不可写，请以管理员身份重新打开后更新")?;
    drop(probe);
    let dir = tempfile::Builder::new()
        .prefix("BetterWX-update-")
        .tempdir()
        .map_err(|e| e.to_string())?
        .keep();
    let helper = dir.join("updater.exe");
    fs::copy(&target, &helper).map_err(|e| e.to_string())?;
    let request = InstallRequest {
        target,
        package: package.to_owned(),
        parent: std::process::id(),
        document: signed.document.clone(),
        signature: signed.signature.clone(),
        ready: dir.join("ready"),
    };
    let path = dir.join("request.json");
    fs::write(&path, serde_json::to_vec(&request).unwrap()).map_err(|e| e.to_string())?;
    task.cancellable.store(false, Ordering::Relaxed);
    task.record("update-prepared", &manifest.version);
    Command::new(helper)
        .arg("--internal-update")
        .arg(&path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(windows)]
fn wait_parent(pid: u32) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return Ok(());
        }
        let result = WaitForSingleObject(handle, 60_000);
        CloseHandle(handle);
        if result != WAIT_OBJECT_0 {
            return Err("等待旧程序退出超时".into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn wait_parent(_pid: u32) -> Result<()> {
    Ok(())
}

pub fn run_helper(path: &Path) -> Result<()> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = current.parent().ok_or("更新目录无效")?;
    if path.parent() != Some(dir)
        || !dir
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("BetterWX-update-"))
        || dir.parent() != Some(std::env::temp_dir().as_path())
    {
        return Err("更新助手目录无效".into());
    }
    let request: InstallRequest =
        serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if download::sha256(&request.target)? != download::sha256(&current)? {
        return Err("更新目标与启动助手不匹配".into());
    }
    let manifest = verify(
        request.document.as_bytes(),
        &request.signature,
        PUBLIC_KEY,
        env!("CARGO_PKG_VERSION"),
    )?;
    if request.target.file_name().is_none()
        || request.target == current
        || request.ready != dir.join("ready")
    {
        return Err("更新目标无效".into());
    }
    wait_parent(request.parent)?;
    if download::sha256(&request.package)? != manifest.sha256 {
        return Err("更新包校验失败".into());
    }
    replace_and_launch(&request.target, &request.package, &request.ready)?;
    download::remove_download(&request.package);
    let _ = fs::remove_file(path);
    Ok(())
}

fn replace_and_launch(target: &Path, package: &Path, ready: &Path) -> Result<()> {
    let parent = target.parent().ok_or("更新目标缺少目录")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let mut input = fs::File::open(package).map_err(|e| e.to_string())?;
    std::io::copy(&mut input, &mut staged).map_err(|e| e.to_string())?;
    staged.as_file().sync_all().map_err(|e| e.to_string())?;
    if download::sha256(staged.path())? != download::sha256(package)? {
        return Err("暂存后的更新包校验失败".into());
    }
    let backup = parent.join(format!(
        ".betterwx-{}.previous.exe",
        uuid::Uuid::new_v4().simple()
    ));
    fs::rename(target, &backup).map_err(|e| e.to_string())?;
    if let Err(e) = staged.persist(target) {
        let _ = fs::rename(&backup, target);
        return Err(e.to_string());
    }
    let outcome = (|| {
        let mut child = Command::new(target)
            .arg("--updated-ready")
            .arg(ready)
            .spawn()
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            if ready.is_file() {
                let version = fs::read_to_string(ready).map_err(|e| e.to_string())?;
                if version.trim() == env!("CARGO_PKG_VERSION") {
                    return Ok(());
                }
                return Err("新版就绪信号版本不匹配".into());
            }
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                return Err("新版启动失败".into());
            }
            if Instant::now() > deadline {
                #[cfg(windows)]
                let _ = Command::new("taskkill.exe")
                    .args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .creation_flags(0x08000000)
                    .status();
                #[cfg(not(windows))]
                let _ = child.kill();
                let _ = child.wait();
                return Err("新版启动超时".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    })();
    if let Err(error) = outcome {
        fs::remove_file(target).map_err(|e| e.to_string())?;
        fs::rename(&backup, target).map_err(|e| e.to_string())?;
        fs::write(ready.with_file_name("rollback.txt"), &error).map_err(|e| e.to_string())?;
        Command::new(target)
            .arg("--rollback-report")
            .arg(ready.with_file_name("rollback.txt"))
            .spawn()
            .map_err(|e| e.to_string())?;
        return Err(error);
    }
    let _ = fs::remove_file(backup);
    let _ = fs::remove_file(ready);
    Ok(())
}

#[cfg(test)]
mod tests;
