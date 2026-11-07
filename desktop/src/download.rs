use crate::task::{Result, Task};
use reqwest::{
    StatusCode,
    blocking::Client,
    header::{ACCEPT_ENCODING, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

pub fn client() -> Result<Client> {
    reqwest::blocking::ClientBuilder::from(
        reqwest::Client::builder()
            .user_agent(concat!("BetterWX/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(8)),
    )
    .timeout(Duration::from_secs(15))
    .build()
    .map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Metadata {
    url: String,
    total: u64,
    validator: Option<String>,
    parts: usize,
}

pub fn cache_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("BetterWX")
        .join("Downloads")
}

pub fn clear_cache() -> Result<()> {
    let root = cache_root();
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 128 * 1024];
    loop {
        let n = file.read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (span, total) = value.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn probe(client: &Client, url: &str) -> Result<(Metadata, bool)> {
    let response = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if response.url().scheme() != "https" && response.url().host_str() != Some("127.0.0.1") {
        return Err("下载地址必须使用 HTTPS".into());
    }
    let validator = response
        .headers()
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.starts_with("W/"))
        .or_else(|| {
            response
                .headers()
                .get(LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
        })
        .map(str::to_owned);
    let ranged = response.status() == StatusCode::PARTIAL_CONTENT;
    let total = if ranged {
        let (a, b, n) = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(range)
            .ok_or("服务端返回了无效 Content-Range")?;
        if a != 0 || b != 0 || n == 0 {
            return Err("探测范围无效".into());
        }
        n
    } else {
        response.content_length().ok_or("服务端未提供安装包长度")?
    };
    if total == 0 || total > 2 * 1024 * 1024 * 1024 {
        return Err("下载文件大小异常".into());
    }
    let parts = if ranged && validator.is_some() && total >= 4 * 1024 * 1024 {
        4
    } else {
        1
    };
    Ok((
        Metadata {
            url: response.url().to_string(),
            total,
            validator,
            parts,
        },
        ranged,
    ))
}

pub fn download(url: &str, expected: Option<(u64, &str)>, task: &Arc<Task>) -> Result<PathBuf> {
    download_to(url, expected, task, &cache_root())
}

pub fn download_to(
    url: &str,
    expected: Option<(u64, &str)>,
    task: &Arc<Task>,
    root: &Path,
) -> Result<PathBuf> {
    download_inner(url, expected, task, root, false)
}

fn download_inner(
    url: &str,
    expected: Option<(u64, &str)>,
    task: &Arc<Task>,
    root: &Path,
    fallback: bool,
) -> Result<PathBuf> {
    let client = client()?;
    task.message("连接下载服务器…");
    let (mut meta, mut ranged) = probe(&client, url)?;
    if fallback {
        meta.parts = 1;
        ranged = false;
    }
    if expected.is_some_and(|(n, _)| n != meta.total) {
        return Err("安装包大小与更新清单不一致".into());
    }
    let key = hex::encode(Sha256::digest(url.as_bytes()));
    let dir = root.join(key);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let metadata = dir.join("download.json");
    let old = fs::read(&metadata)
        .ok()
        .and_then(|b| serde_json::from_slice::<Metadata>(&b).ok());
    if !old.as_ref().is_some_and(|old| {
        old.total == meta.total && old.validator == meta.validator && old.parts == meta.parts
    }) || meta.validator.is_none()
        || !ranged
    {
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_file() {
                fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
    }
    fs::write(&metadata, serde_json::to_vec(&meta).unwrap()).map_err(|e| e.to_string())?;
    let ranges: Vec<_> = (0..meta.parts)
        .map(|i| {
            let start = meta.total * i as u64 / meta.parts as u64;
            let end = meta.total * (i + 1) as u64 / meta.parts as u64 - 1;
            (start, end)
        })
        .collect();
    let mut lengths = Vec::new();
    for (i, (start, end)) in ranges.iter().enumerate() {
        let part = dir.join(format!("{i}.part"));
        let n = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let hash_file = dir.join(format!("{i}.sha256"));
        let valid = n <= end - start + 1
            && (n == 0
                || fs::read_to_string(&hash_file)
                    .ok()
                    .is_some_and(|h| sha256(&part).ok().as_ref() == Some(&h)));
        if !valid {
            let _ = fs::remove_file(&part);
            let _ = fs::remove_file(hash_file);
            lengths.push(0);
        } else {
            lengths.push(n);
        }
    }
    let bytes = Arc::new(AtomicU64::new(lengths.iter().sum()));
    task.baseline(bytes.load(Ordering::Relaxed));
    task.download(bytes.load(Ordering::Relaxed), meta.total);
    task.record(
        "download",
        &format!(
            "{}; {} bytes; {} connections; resume={}",
            meta.url,
            meta.total,
            meta.parts,
            bytes.load(Ordering::Relaxed)
        ),
    );
    task.message("正在下载，可暂停后继续");
    let error = Mutex::new(None);
    thread::scope(|scope| {
        for (i, (start, end)) in ranges.iter().copied().enumerate() {
            let client = &client;
            let meta = &meta;
            let dir = &dir;
            let bytes = &bytes;
            let error = &error;
            scope.spawn(move || {
                let part = dir.join(format!("{i}.part"));
                let result = (|| {
                    for attempt in 0..4 {
                        task.progress.check().map_err(|e| e.to_string())?;
                        let result = segment(client, meta, ranged, &part, start, end, bytes, task);
                        match result {
                            Ok(()) => return Ok(()),
                            Err(e)
                                if attempt == 3
                                    || task.progress.check().is_err()
                                    || e.starts_with("协议错误") =>
                            {
                                return Err(e);
                            }
                            Err(e) => {
                                task.record("retry", &format!("segment {i}: {e}"));
                                for _ in 0..(10 * (1 << attempt)) {
                                    task.progress.check().map_err(|e| e.to_string())?;
                                    thread::sleep(Duration::from_millis(100));
                                }
                            }
                        }
                    }
                    unreachable!()
                })();
                if part.exists() {
                    if let Ok(hash) = sha256(&part) {
                        let _ = fs::write(dir.join(format!("{i}.sha256")), hash);
                    }
                }
                if let Err(e) = result {
                    *error.lock().unwrap() = Some(e);
                }
            });
        }
    });
    if let Some(e) = error.into_inner().unwrap() {
        if e.starts_with("协议错误") && !fallback && task.progress.check().is_ok() {
            task.record("fallback", "服务器不支持可靠的范围下载，切换单连接");
            return download_inner(url, expected, task, root, true);
        }
        return Err(e);
    }
    task.progress.check().map_err(|e| e.to_string())?;
    let output = dir.join("installer.exe");
    let mut file = File::create(&output).map_err(|e| e.to_string())?;
    for i in 0..meta.parts {
        let mut part = File::open(dir.join(format!("{i}.part"))).map_err(|e| e.to_string())?;
        std::io::copy(&mut part, &mut file).map_err(|e| e.to_string())?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    if fs::metadata(&output).map_err(|e| e.to_string())?.len() != meta.total {
        return Err("下载长度校验失败".into());
    }
    if let Some((_, hash)) = expected {
        if sha256(&output)? != hash {
            let _ = fs::remove_dir_all(&dir);
            return Err("SHA-256 校验失败，请重新下载".into());
        }
    }
    task.record("verified-download", &sha256(&output)?);
    Ok(output)
}

fn segment(
    client: &Client,
    meta: &Metadata,
    ranged: bool,
    path: &Path,
    start: u64,
    end: u64,
    bytes: &AtomicU64,
    task: &Task,
) -> Result<()> {
    let mut n = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if n == end - start + 1 {
        return Ok(());
    }
    if !ranged && n > 0 {
        fs::remove_file(path).map_err(|e| e.to_string())?;
        bytes.fetch_sub(n, Ordering::Relaxed);
        n = 0;
    }
    let mut request = client.get(&meta.url).header(ACCEPT_ENCODING, "identity");
    if ranged {
        request = request.header(RANGE, format!("bytes={}-{}", start + n, end));
        if let Some(v) = &meta.validator {
            request = request.header(IF_RANGE, v);
        }
    }
    let mut response = request
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    task.record(
        "response",
        &format!("segment={start}-{end}; status={}", response.status()),
    );
    if ranged
        && (response.status() != StatusCode::PARTIAL_CONTENT
            || response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(range)
                != Some((start + n, end, meta.total)))
    {
        return Err("协议错误：服务器未返回请求范围，请重新下载".into());
    }
    if ranged
        && response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| {
                meta.validator
                    .as_ref()
                    .is_some_and(|expected| expected.starts_with('"') && value != expected)
            })
    {
        return Err("协议错误：下载期间远端文件变化".into());
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut buffer = [0; 64 * 1024];
    loop {
        task.progress.check().map_err(|e| e.to_string())?;
        let count = response.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if n + count as u64 > end - start + 1 {
            return Err("协议错误：下载分段越界".into());
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        n += count as u64;
        let total = bytes.fetch_add(count as u64, Ordering::Relaxed) + count as u64;
        task.download(total, meta.total);
    }
    file.sync_all().map_err(|e| e.to_string())?;
    if n != end - start + 1 {
        return Err("下载连接提前结束".into());
    }
    Ok(())
}

pub fn remove_download(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod tests;
