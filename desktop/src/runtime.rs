use crate::{
    download,
    task::{Result, Task},
};
use serde::Serialize;
use std::{
    process::Command,
    sync::{Arc, atomic::Ordering},
};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
};

pub const DOWNLOAD_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124701";
pub const MIN_MAJOR: u64 = 120;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub version: Option<String>,
    pub compatible: bool,
}

pub fn status() -> Runtime {
    let candidates = [
        (
            HKEY_CURRENT_USER,
            r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        ),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        ),
    ];
    let version = candidates
        .into_iter()
        .filter_map(|(h, p)| {
            RegKey::predef(h)
                .open_subkey(p)
                .ok()?
                .get_value::<String, _>("pv")
                .ok()
        })
        .filter(|v| v != "0.0.0.0")
        .max_by_key(|v| {
            v.split('.')
                .map(|p| p.parse::<u64>().unwrap_or(0))
                .collect::<Vec<_>>()
        });
    let compatible = version
        .as_ref()
        .and_then(|v| v.split('.').next()?.parse::<u64>().ok())
        .is_some_and(|v| v >= MIN_MAJOR);
    Runtime {
        version,
        compatible,
    }
}

pub fn install(task: &Arc<Task>) -> Result<serde_json::Value> {
    let path = download::download(DOWNLOAD_URL, None, task)?;
    task.message("校验微软安装包签名…");
    let code = " $s=Get-AuthenticodeSignature -LiteralPath $env:BETTERWX_VERIFY_FILE; if ($s.Status -eq 'Valid' -and $s.SignerCertificate.Subject -match '(^|,\\s*)CN=Microsoft Corporation(,|$)') { exit 0 } else { exit 1 }";
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", code])
        .env("BETTERWX_VERIFY_FILE", &path)
        .creation_flags_no_window()
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        download::remove_download(&path);
        return Err("微软安装包签名验证失败，已删除下载文件".into());
    }
    task.progress.check().map_err(|e| e.to_string())?;
    task.cancellable.store(false, Ordering::Relaxed);
    task.message("正在安装 WebView2，请等待系统安装程序完成…");
    task.record("install-start", "Microsoft Evergreen x64");
    let exit = Command::new(&path)
        .args(["/silent", "/install"])
        .creation_flags_no_window()
        .status()
        .map_err(|e| e.to_string())?;
    let runtime = status_after_install();
    task.record(
        "install-exit",
        &format!("code={:?}; version={:?}", exit.code(), runtime.version),
    );
    if !matches!(exit.code(), Some(0 | 3010)) || !runtime.compatible {
        return Err(format!(
            "安装后未检测到可用运行时，安装程序返回 {:?}。请重试或检查系统安装策略",
            exit.code()
        ));
    }
    download::remove_download(&path);
    serde_json::to_value(runtime).map_err(|e| e.to_string())
}

fn status_after_install() -> Runtime {
    status()
}

trait HiddenCommand {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}
impl HiddenCommand for Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x08000000)
    }
}
