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
