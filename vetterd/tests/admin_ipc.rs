//! Integration tests for the admin socket (`vet daemon list` IPC).
//!
//! Uses `VETTERD_NOTIFIER=noop` so prompt-class requests park in the
//! pending queue indefinitely — no mock UI race to contend with. The
//! daemon is SIGTERM'd at the end, which triggers `cancel_all()` and
//! unblocks the parked connection workers with a deny.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use vetter_core::wire::{
    new_request_id, read_decision, read_frame, write_frame, MgmtRequest, MgmtResponse, VetRequest,
    PROTOCOL_VERSION,
};

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

unsafe fn libc_kill(pid: i32, sig: i32) {
    let _ = kill(pid, sig);
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("socket did not appear at {}", path.display());
}

fn send_mgmt(path: &Path, req: MgmtRequest) -> MgmtResponse {
    let mut stream = UnixStream::connect(path).expect("connect to admin socket");
    write_frame(&mut stream, &req).expect("write MgmtRequest");
    read_frame::<_, MgmtResponse>(&mut stream).expect("read MgmtResponse")
}

const EMPTY_ALLOWLIST: &str = "rules: []\ndeny: []\n";

fn spawn_noop_daemon(
    scratch: &TempDir,
) -> (std::process::Child, std::path::PathBuf, std::path::PathBuf) {
    let socket = scratch.path().join("vetter.sock");
    let admin_socket = scratch.path().join("vetter-admin.sock");
    let audit = scratch.path().join("audit.log");
    let allow = scratch.path().join("allowlist.yaml");
    std::fs::write(&allow, EMPTY_ALLOWLIST).unwrap();

    let child = Command::new(assert_cmd::cargo_bin!("vetterd"))
        .env("VETTERD_SOCKET", &socket)
        .env("VETTER_AUDIT_LOG", &audit)
        .env("VETTER_ALLOWLIST", &allow)
        .env("VETTERD_NOTIFIER", "noop")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn vetterd");

    wait_for_socket(&socket, Duration::from_secs(5));
    wait_for_socket(&admin_socket, Duration::from_secs(2));

    (child, socket, admin_socket)
}

fn sigterm_and_wait(mut child: std::process::Child) {
    unsafe { libc_kill(child.id() as i32, 15) };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return,
        }
    }
}

/// A freshly started daemon with no pending requests returns an empty list.
#[test]
fn admin_list_pending_empty_when_idle() {
    let scratch = TempDir::new().unwrap();
    let (child, _socket, admin_socket) = spawn_noop_daemon(&scratch);

    let resp = send_mgmt(&admin_socket, MgmtRequest::ListPending);
    match resp {
        MgmtResponse::PendingList { items } => {
            assert!(items.is_empty(), "expected empty list, got {:?}", items);
        }
        other => panic!("unexpected admin response: {:?}", other),
    }

    sigterm_and_wait(child);
}

/// Requests parked by the noop notifier appear in the pending list
/// with the correct fields, and disappear once the daemon shuts down.
#[test]
fn admin_list_pending_shows_parked_requests() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    // Submit a curl request in a background thread. The noop notifier
    // never resolves it so the worker blocks until cancel_all().
    let socket_clone = socket.clone();
    let req = VetRequest {
        v: PROTOCOL_VERSION,
        id: new_request_id(),
        cwd: None,
        agent_hint: None,
        argv: vec!["curl".into(), "https://api.example.test/v1/data".into()],
        force_prompt: false,
    };
    let req_id = req.id.clone();
    let handle = std::thread::spawn(move || {
        let mut s = UnixStream::connect(&socket_clone).expect("connect main socket");
        write_frame(&mut s, &req).expect("write VetRequest");
        read_decision(&mut s, &req.id)
    });

    // Poll the admin socket until the item appears (max 2 s).
    let items = {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let resp = send_mgmt(&admin_socket, MgmtRequest::ListPending);
            if let MgmtResponse::PendingList { items } = resp {
                if !items.is_empty() {
                    break items;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for pending item to appear"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    assert_eq!(items.len(), 1, "expected 1 pending item, got {:?}", items);
    let item = &items[0];
    assert_eq!(item.id, req_id, "id mismatch");
    assert_eq!(item.command, "curl");
    assert_eq!(item.primary_verb, "GET");
    assert!(
        item.primary_target.contains("api.example.test"),
        "unexpected primary_target: {}",
        item.primary_target
    );
    assert!(!item.force_prompt);

    // SIGTERM → cancel_all() → the pending worker wakes with a deny.
    sigterm_and_wait(child);

    // Drain the handle; don't assert on the decision payload because
    // the daemon may exit before the worker finishes writing its deny
    // response (acceptable race — no worker join at daemon shutdown).
    let _ = handle.join();
}

/// Multiple concurrent parked requests all appear in the list.
#[test]
fn admin_list_pending_multiple_requests() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let urls = [
        "https://a.example.test/",
        "https://b.example.test/",
        "https://c.example.test/",
    ];
    let mut req_ids = Vec::new();
    let mut handles = Vec::new();

    for url in &urls {
        let socket_clone = socket.clone();
        let req = VetRequest {
            v: PROTOCOL_VERSION,
            id: new_request_id(),
            cwd: None,
            agent_hint: None,
            argv: vec!["curl".into(), (*url).into()],
            force_prompt: false,
        };
        req_ids.push(req.id.clone());
        handles.push(std::thread::spawn(move || {
            let mut s = UnixStream::connect(&socket_clone).expect("connect");
            write_frame(&mut s, &req).expect("write");
            read_decision(&mut s, &req.id)
        }));
    }

    // Poll until all three requests appear in the queue.
    let items = {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let resp = send_mgmt(&admin_socket, MgmtRequest::ListPending);
            if let MgmtResponse::PendingList { items } = resp {
                if items.len() == urls.len() {
                    break items;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {} pending items",
                urls.len()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    assert_eq!(items.len(), urls.len());
    for id in &req_ids {
        assert!(
            items.iter().any(|i| &i.id == id),
            "id {id} missing from pending list: {:?}",
            items
        );
    }

    sigterm_and_wait(child);
    // Just drain the handles; don't assert on the decision payload
    // because the daemon may exit before all workers write their deny
    // responses (no worker join at daemon shutdown — acceptable race).
    for h in handles {
        let _ = h.join();
    }
}
