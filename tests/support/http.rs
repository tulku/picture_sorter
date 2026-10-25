use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc::{self, Receiver},
    thread,
};
pub fn serve(
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
) -> (url::Url, Receiver<String>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while request.len() < 32768 {
            if stream.read(&mut byte).unwrap_or(0) == 0 {
                break;
            }
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8(request).unwrap();
        tx.send(request).unwrap();
        let extra = headers
            .into_iter()
            .map(|(key, value)| format!("{key}: {value}\r\n"))
            .collect::<String>();
        write!(stream,"HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",body.len()).unwrap();
    });
    (
        url::Url::parse(&format!("http://{address}")).unwrap(),
        rx,
        handle,
    )
}
#[derive(Clone)]
pub struct RatingServerState {
    pub rating: std::sync::Arc<std::sync::Mutex<Option<i8>>>,
    pub version: std::sync::Arc<std::sync::Mutex<String>>,
    pub writes: std::sync::Arc<std::sync::Mutex<Vec<Option<i8>>>>,
    pub write_status: std::sync::Arc<std::sync::atomic::AtomicU16>,
    pub drop_put: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
pub struct RatingServer {
    pub origin: url::Url,
    pub asset: uuid::Uuid,
    pub state: RatingServerState,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl RatingServer {
    pub fn new() -> Self {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin =
            url::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let asset = uuid::Uuid::new_v4();
        let state = RatingServerState {
            rating: Arc::new(Mutex::new(None)),
            version: Arc::new(Mutex::new("3.3.5".into())),
            writes: Arc::new(Mutex::new(Vec::new())),
            write_status: Arc::new(std::sync::atomic::AtomicU16::new(200)),
            drop_put: Arc::new(AtomicBool::new(false)),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let stop_worker = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stop_worker.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .unwrap();
                let mut header = Vec::new();
                let mut byte = [0u8; 1];
                while header.len() < 32768 {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    header.push(byte[0]);
                    if header.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let header = String::from_utf8(header).unwrap();
                let length = header
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|n| n.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                if stream.read_exact(&mut body).is_err() {
                    continue;
                }
                let response = if header.starts_with("GET /api/server/about ") {
                    serde_json::json!({"version":*worker_state.version.lock().unwrap(),"versionUrl":"https://fixture.invalid","licensed":false})
                } else {
                    if header.starts_with("PUT /api/assets/") {
                        let status = worker_state.write_status.load(Ordering::SeqCst);
                        if status != 200 {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 {status} Denied\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                            );
                            continue;
                        }

                        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let rating = value["rating"].as_i64().map(|n| n as i8);
                        *worker_state.rating.lock().unwrap() = rating;
                        worker_state.writes.lock().unwrap().push(rating);
                        if worker_state.drop_put.swap(false, Ordering::SeqCst) {
                            continue;
                        }
                    }
                    let mut asset_value: serde_json::Value =
                        serde_json::from_str(include_str!("../fixtures/immich/asset.json"))
                            .unwrap();
                    asset_value["id"] = serde_json::json!(asset);
                    asset_value["exifInfo"]["rating"] =
                        serde_json::json!(*worker_state.rating.lock().unwrap());
                    asset_value
                };
                let body = serde_json::to_string(&response).unwrap();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            origin,
            asset,
            state,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for RatingServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
