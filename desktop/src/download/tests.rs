use super::*;
use std::{
    io::BufRead,
    net::{TcpListener, TcpStream},
    sync::atomic::AtomicBool,
};

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Option<(u64, u64)>>>>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(
            self.url
                .trim_start_matches("http://")
                .trim_end_matches("/file"),
        );
        if let Some(h) = self.handle.take() {
            h.join().unwrap();
        }
    }
}
fn serve(data: Vec<u8>, ranged: bool, ignore: bool, slow: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let stopping = stop.clone();
    let data = Arc::new(data);
    let handle = thread::spawn(move || {
        for stream in listener.incoming() {
            if stopping.load(Ordering::Relaxed) {
                break;
            }
            let mut stream = stream.unwrap();
            let data = data.clone();
            let seen = seen.clone();
            thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut header = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    header.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let requested = header
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("range: bytes=")
                            .map(str::to_owned)
                    })
                    .and_then(|r| {
                        let (a, b) = r.split_once('-')?;
                        Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
                    });
                seen.lock().unwrap().push(requested);
                let requested = requested.filter(|r| ranged && (!ignore || *r == (0, 0)));
                let (start, end) = requested.unwrap_or((0, data.len() as u64 - 1));
                let response = if requested.is_some() {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\n",
                        data.len()
                    )
                } else {
                    "HTTP/1.1 200 OK\r\n".into()
                };
                if write!(stream,"{response}Content-Length: {}\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\n",end-start+1).is_err(){return;}
                for chunk in data[start as usize..=end as usize].chunks(32768) {
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    if slow {
                        thread::sleep(Duration::from_millis(3));
                    }
                }
            });
        }
    });
    Server {
        url: format!("http://{address}/file"),
        stop,
        requests,
        handle: Some(handle),
    }
}
fn payload() -> Vec<u8> {
    (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect()
}

#[test]
fn four_ranges_produce_verified_file() {
    let data = payload();
    let hash = hex::encode(Sha256::digest(&data));
    let server = serve(data, true, false, false);
    let dir = tempfile::tempdir().unwrap();
    let task = Task::new("download", dir.path()).unwrap();
    let path = download_to(
        &server.url,
        Some((5 * 1024 * 1024, &hash)),
        &task,
        &dir.path().join("cache"),
    )
    .unwrap();
    assert_eq!(sha256(&path).unwrap(), hash);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests.iter().all(Option::is_some));
}

#[test]
fn server_without_ranges_uses_single_stream() {
    let data = payload();
    let hash = hex::encode(Sha256::digest(&data));
    let server = serve(data, false, false, false);
    let dir = tempfile::tempdir().unwrap();
    let task = Task::new("download", dir.path()).unwrap();
    let path = download_to(&server.url, None, &task, &dir.path().join("cache")).unwrap();
    assert_eq!(sha256(&path).unwrap(), hash);
    assert!(server.requests.lock().unwrap().contains(&None));
}

#[test]
fn ignored_ranges_fall_back_without_corrupting_file() {
    let data = payload();
    let hash = hex::encode(Sha256::digest(&data));
    let server = serve(data, true, true, false);
    let dir = tempfile::tempdir().unwrap();
    let task = Task::new("download", dir.path()).unwrap();
    let path = download_to(&server.url, None, &task, &dir.path().join("cache")).unwrap();
    assert_eq!(sha256(&path).unwrap(), hash);
    assert!(server.requests.lock().unwrap().contains(&None));
}

#[test]
fn pause_and_new_operation_resume_only_missing_bytes() {
    let data = payload();
    let hash = hex::encode(Sha256::digest(&data));
    let server = serve(data, true, false, true);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let task = Task::new("download", dir.path()).unwrap();
    let watcher = task.clone();
    let cancel = thread::spawn(move || {
        while watcher.snapshot().bytes < 256 * 1024 {
            thread::sleep(Duration::from_millis(2));
        }
        watcher.progress.cancel();
    });
    assert!(download_to(&server.url, None, &task, &root).is_err());
    cancel.join().unwrap();
    let old_count = server.requests.lock().unwrap().len();
    let next = Task::new("resume", dir.path()).unwrap();
    let path = download_to(&server.url, None, &next, &root).unwrap();
    assert_eq!(sha256(&path).unwrap(), hash);
    let seen = server.requests.lock().unwrap();
    let next_ranges = &seen[old_count + 1..];
    assert!(
        next_ranges
            .iter()
            .flatten()
            .any(|(start, _)| start % (5 * 1024 * 1024 / 4) != 0)
    );
}

#[test]
fn hash_mismatch_and_invalid_destination_are_rejected() {
    let server = serve(payload(), true, false, false);
    let dir = tempfile::tempdir().unwrap();
    let task = Task::new("download", dir.path()).unwrap();
    assert!(
        download_to(
            &server.url,
            Some((5 * 1024 * 1024, &"0".repeat(64))),
            &task,
            &dir.path().join("cache")
        )
        .unwrap_err()
        .contains("SHA-256")
    );
    let file = dir.path().join("not-a-directory");
    fs::write(&file, b"x").unwrap();
    assert!(download_to(&server.url, None, &task, &file).is_err());
}
