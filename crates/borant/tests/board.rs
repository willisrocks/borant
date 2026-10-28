//! `borant board` names the session it runs in, so the hub gives a chief the
//! board of its own collection and a person every collection.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

/// One request of the stub host: its path and body.
struct Request {
    path: String,
    body: String,
}

/// Serves up to `count` requests, answering each with `respond`, and stops at
/// the count or after five seconds. Returns what the CLI sent, in order.
fn serve(
    listener: UnixListener,
    count: usize,
    respond: impl Fn(&Request) -> String + Send + 'static,
) -> std::thread::JoinHandle<Vec<Request>> {
    use std::io::ErrorKind;
    use std::time::{Duration, Instant};

    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut out = Vec::new();
        while out.len() < count && Instant::now() < deadline {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("the stub host could not accept: {e}"),
            };
            stream.set_nonblocking(false).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
                if head.is_empty() {
                    head = line;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request = Request {
                path: head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned(),
                body: String::from_utf8_lossy(&body).into_owned(),
            };
            let reply = respond(&request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                reply.len(),
                reply
            )
            .unwrap();
            stream.flush().unwrap();
            out.push(request);
        }
        out
    })
}

/// The reply of a host that knows the session, and the board of one collection.
fn known_session(_: &Request) -> String {
    r#"{"ok":{"session":"01M60B5C7C4H0FBFZ9B7ZXM442","agent":"chief","host":"laptop","state":"attached","movable":true,"channels":[]}}"#.to_owned()
}

fn the_board(_: &Request) -> String {
    r#"{"ok":{"at":0,"collections":[]}}"#.to_owned()
}

fn run_board(dir: &Path, socket: &Path, agent: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_borant"));
    cmd.args(["board", "--json"])
        .env("BORANT_HOME", dir)
        .env("BORANT_SOCKET", socket)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BORANT_AGENT");
    if let Some(agent) = agent {
        cmd.env("BORANT_AGENT", agent);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

/// A chief session runs `borant board` inside its harness, so the call names
/// the session and the hub filters the board to the agent's collection.
#[test]
fn board_names_the_session_it_runs_in() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = serve(listener, 2, |r| match r.path.as_str() {
        "/v1/resolve" => known_session(r),
        _ => the_board(r),
    });
    let out = run_board(dir.path(), &socket, Some("chief-1"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let shown: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(shown["collections"], serde_json::json!([]));

    let requests = server.join().unwrap();
    assert_eq!(requests[0].path, "/v1/resolve");
    let asked: Value = serde_json::from_str(&requests[0].body).unwrap();
    assert!(asked["ancestors"].is_array(), "{}", requests[0].body);

    assert_eq!(requests[1].path, "/v1/call");
    let call: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(call["session"], "01M60B5C7C4H0FBFZ9B7ZXM442");
    assert_eq!(call["call"]["tool"], "board");
}

/// A person's shell has no session, so the same call stays the person's and
/// still reads the board.
#[test]
fn board_without_a_session_is_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = serve(listener, 2, |r| match r.path.as_str() {
        "/v1/resolve" => {
            r#"{"error":{"code":"not_found","message":"no local session for these processes"}}"#
                .to_owned()
        }
        _ => the_board(r),
    });
    let out = run_board(dir.path(), &socket, None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = server.join().unwrap();
    assert_eq!(requests[0].path, "/v1/resolve");
    assert_eq!(requests[1].path, "/v1/call");
    let call: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert!(call.get("session").is_none(), "{}", requests[1].body);
    assert_eq!(call["call"]["tool"], "board");
}

/// A launched session carries BORANT_AGENT. When its session does not resolve,
/// the board refuses to read as the person, whose read lists every collection.
#[test]
fn board_refuses_to_be_the_person_when_in_a_session() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = serve(listener, 1, |_| {
        r#"{"error":{"code":"not_found","message":"no local session for these processes"}}"#
            .to_owned()
    });
    let out = run_board(dir.path(), &socket, Some("chief-1"));
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to read the board as the person"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());

    // Only the resolve: nothing read the board as the person.
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/resolve");
}
