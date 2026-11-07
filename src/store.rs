use std::{
    collections::{BTreeMap, btree_map::Entry},
    fs::{self, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Component, Path, PathBuf, Prefix},
    sync::Arc,
};

use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{Result, data::Data, progress::Progress};

pub struct Store {
    pub target: PathBuf,
    pub files: BTreeMap<PathBuf, File>,
}

pub struct File {
    pub target: PathBuf,
    backup: PathBuf,
    legacy_backup: PathBuf,
    pub current: Data,
    pub original: Arc<Vec<u8>>,
    pub enabled: BTreeMap<String, usize>,
    pub issue: Option<String>,
    pub source_issue: Option<String>,
    pub confirmed: bool,
}

pub struct Content {
    pub data: Data,
    pub enabled: BTreeMap<String, usize>,
}

pub fn display(path: &Path) -> String {
    let text = path.to_string_lossy();
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(_) => text[4..].to_owned(),
            Prefix::VerbatimUNC(..) => format!(r"\\{}", &text[8..]),
            _ => text.into_owned(),
        },
        _ => text.into_owned(),
    }
}

fn read(path: &Path, progress: &Progress, name: &str) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let total = file.metadata()?.len();
    let mut data = Vec::with_capacity(total as usize);
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        progress.check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..count]);
    }
    progress.record(
        name,
        &path.file_name().unwrap_or_default().to_string_lossy(),
    );
    Ok(data)
}

pub fn write(path: &Path, data: &[u8], progress: &Progress) -> Result<()> {
    write_parts(path, data.len(), [data], progress)
}

fn write_parts<'a>(
    path: &Path,
    size: usize,
    parts: impl IntoIterator<Item = &'a [u8]>,
    progress: &Progress,
) -> Result<()> {
    let parent = path.parent().ok_or("目标文件缺少父目录")?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let step = progress.start("写入文件", size as u64, "字节");
    step.detail(name.as_ref());
    let mut completed = 0;
    {
        let mut output = BufWriter::with_capacity(1024 * 1024, temporary.as_file_mut());
        for part in parts {
            for chunk in part.chunks(1024 * 1024) {
                progress.check()?;
                output.write_all(chunk)?;
                completed += chunk.len();
                step.set(completed as u64);
            }
        }
        output.flush()?;
    }
    temporary.as_file().sync_all()?;
    progress.check()?;
    match temporary.persist(path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::CrossesDevices => {
            let source = error.file.path().to_owned();
            fs::copy(&source, path)?;
            error.file.close()?;
        }
        Err(error) => return Err(error.error.into()),
    }
    step.finish(format!("{name}；{size} 字节"));
    Ok(())
}

#[cfg(windows)]
fn signature(path: &Path) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr::from_mut};

    use windows::core::{Error, HRESULT};
    use windows_sys::Win32::{
        Foundation::{INVALID_HANDLE_VALUE, TRUST_E_BAD_DIGEST, TRUST_E_NOSIGNATURE},
        Security::WinTrust::{
            WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
            WTD_CHOICE_FILE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
            WinVerifyTrust,
        },
    };

    let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path.as_ptr(),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &mut action,
            from_mut(&mut data).cast(),
        )
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &mut action,
            from_mut(&mut data).cast(),
        );
    }
    match status {
        0 => Ok(()),
        TRUST_E_BAD_DIGEST => Err("文件内容存在改动".into()),
        TRUST_E_NOSIGNATURE => Err("文件缺少校验信息".into()),
        _ => Err(format!("无法检查文件改动：{}", Error::from_hresult(HRESULT(status)),).into()),
    }
}

#[cfg(not(windows))]
fn signature(_path: &Path) -> Result<()> {
    Err("检查文件改动需要 Windows".into())
}

impl Store {
    pub fn open(target: &Path, progress: &Progress) -> Result<Self> {
        let target = fs::canonicalize(target)?;
        let mut store = Self {
            target: target.clone(),
            files: BTreeMap::new(),
        };
        store.load(&target, progress)?;
        Ok(store)
    }

    pub fn load(&mut self, target: &Path, progress: &Progress) -> Result<&File> {
        let target = fs::canonicalize(target)?;
        let backup = self.backup(&target)?;
        let legacy_backup = legacy_backup_path(&target);
        Ok(match self.files.entry(target) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let file = File::open(entry.key(), backup, legacy_backup, progress)?;
                entry.insert(file)
            }
        })
    }

    fn backup(&self, target: &Path) -> Result<PathBuf> {
        backup_path(target)
    }

    pub fn write_error(&self, targets: &[PathBuf]) -> Option<String> {
        targets.iter().find_map(|target| {
            OpenOptions::new()
                .write(true)
                .open(target)
                .err()
                .map(|error| format!("无法写入 {}：{error}", display(target)))
        })
    }

    pub fn load_backups(&mut self, targets: &[PathBuf], progress: &Progress) -> Result<()> {
        for target in targets {
            if target.try_exists()? && backup_exists_for(target)? {
                self.load(target, progress)?;
            }
        }
        Ok(())
    }

    pub fn backup_path(target: &Path) -> Result<PathBuf> {
        backup_path(target)
    }

    pub fn backup_exists(target: &Path) -> Result<bool> {
        backup_exists_for(target)
    }

    pub fn changed(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|(_, file)| {
                file.current.matches(0, &file.original) != Some(true)
                    || file.current.len() != file.original.len()
            })
            .map(|(target, _)| target.clone())
            .collect()
    }

    pub fn originals(&self) -> Result<BTreeMap<PathBuf, Content>> {
        let changed = self.changed();
        if changed.is_empty()
            && let Some(issue) = self
                .files
                .values()
                .find_map(|file| file.source_issue.as_ref())
        {
            return Err(issue.clone().into());
        }
        Ok(changed
            .into_iter()
            .map(|target| {
                let data = Data::from(self.files[&target].original.clone());
                (
                    target,
                    Content {
                        data,
                        enabled: BTreeMap::new(),
                    },
                )
            })
            .collect())
    }

    pub fn save(
        &mut self,
        contents: BTreeMap<PathBuf, Content>,
        progress: &Progress,
    ) -> Result<()> {
        for (target, content) in &contents {
            let file = &self.files[target];
            if file.current != content.data
                && !file.confirmed
                && let Some(issue) = &file.source_issue
            {
                return Err(issue.clone().into());
            }
        }
        for (target, content) in contents {
            progress.check()?;
            self.files
                .get_mut(&target)
                .expect("file not loaded")
                .save(content, progress)?;
        }
        Ok(())
    }
}

fn backup_path(target: &Path) -> Result<PathBuf> {
    let target = normalized_target(target);
    let root = backup_root();
    let name = target.file_name().ok_or("目标文件缺少名称")?;
    Ok(root
        .join(target_id(&target))
        .join(name)
        .with_added_extension("bak"))
}

fn legacy_backup_path(target: &Path) -> PathBuf {
    target.with_added_extension("bak")
}

fn normalized_target(target: &Path) -> PathBuf {
    fs::canonicalize(target).unwrap_or_else(|_| target.to_owned())
}

fn backup_exists_for(target: &Path) -> Result<bool> {
    Ok(backup_path(target)?.try_exists()? || legacy_backup_path(target).try_exists()?)
}

fn target_id(target: &Path) -> String {
    let normalized = target.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let normalized = normalized.to_ascii_lowercase();
    Sha256::digest(normalized.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn backup_root() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|home| PathBuf::from(home).join("AppData/Local").into_os_string())
        })
        .unwrap_or_else(|| PathBuf::from(".").into_os_string());
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| {
            PathBuf::from(home)
                .join("Library/Application Support")
                .into_os_string()
        })
        .unwrap_or_else(|| PathBuf::from(".").into_os_string());
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local/share").into_os_string())
        })
        .unwrap_or_else(|| PathBuf::from(".").into_os_string());
    PathBuf::from(base).join("BetterWX").join("backups")
}

impl File {
    pub fn backed_up(&self) -> Result<bool> {
        Ok(self.backup.try_exists()? || self.legacy_backup.try_exists()?)
    }

    fn open(
        target: &Path,
        backup: PathBuf,
        legacy_backup: PathBuf,
        progress: &Progress,
    ) -> Result<Self> {
        let mut source_issue = None;
        let source_backup = if backup.try_exists()? {
            Some(backup.as_path())
        } else if legacy_backup.try_exists()? {
            Some(legacy_backup.as_path())
        } else {
            None
        };
        let (original, current) = if let Some(source_backup) = source_backup {
            let original = Arc::new(read(source_backup, progress, "载入文件")?);
            let mut file = fs::File::open(target)?;
            let mut current = Data::default();
            let mut buffer = vec![0; 1024 * 1024];
            let mut changed = Vec::new();
            let mut offset = 0;
            loop {
                progress.check()?;
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                for bytes in buffer[..count].chunks(4096) {
                    let end = offset + bytes.len();
                    if original.get(offset..end) == Some(bytes) {
                        if !changed.is_empty() {
                            let size = changed.len();
                            current.push(Arc::new(std::mem::take(&mut changed)), 0..size);
                        }
                        current.push(original.clone(), offset..end);
                    } else {
                        changed.extend_from_slice(bytes);
                    }
                    offset = end;
                }
            }
            let size = changed.len();
            current.push(Arc::new(changed), 0..size);
            progress.record(
                "载入文件",
                &target.file_name().unwrap_or_default().to_string_lossy(),
            );
            (original, current)
        } else {
            let original = Arc::new(read(target, progress, "载入文件")?);
            let step = progress.activity("检查文件改动");
            step.detail(display(target));
            source_issue = signature(target)
                .err()
                .map(|error| format!("{} 缺少原版备份，{error}", display(target)));
            step.finish(source_issue.as_deref().unwrap_or("未发现改动"));
            let current = Data::from(original.clone());
            (original, current)
        };
        Ok(Self {
            target: target.to_owned(),
            backup,
            legacy_backup,
            current,
            original,
            enabled: BTreeMap::new(),
            issue: None,
            source_issue,
            confirmed: false,
        })
    }

    fn save(&mut self, content: Content, progress: &Progress) -> Result<()> {
        if self.current == content.data {
            return Ok(());
        }
        let mut file = fs::File::open(&self.target)?;
        let mut buffer = vec![0; 1024 * 1024];
        let mut offset = 0;
        let unchanged = loop {
            progress.check()?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break offset == self.current.len();
            }
            if self.current.matches(offset, &buffer[..count]) != Some(true) {
                break false;
            }
            offset += count;
        };
        drop(file);
        progress.record(
            "读取当前文件",
            &self
                .target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
        );
        if !unchanged {
            return Err(format!("读取后目标文件发生了变化：{}", display(&self.target)).into());
        }
        if !self.backup.try_exists()? {
            fs::create_dir_all(self.backup.parent().ok_or("备份文件缺少父目录")?)?;
            write(&self.backup, &self.original, progress)?;
        }
        write_parts(
            &self.target,
            content.data.len(),
            content.data.chunks(),
            progress,
        )?;
        self.current = content.data;
        self.enabled = content.enabled;
        self.issue = None;
        self.source_issue = None;
        Ok(())
    }
}
