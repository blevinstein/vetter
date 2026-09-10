//! End-to-end smoke of the real [`LinuxNotifier`] over a private
//! D-Bus session bus.
//!
//! Every other automated test of the prompt path drives
//! `MockNotifier`, which speaks a Unix-socket protocol of our own
//! invention. That covers the *queue* round-trip but never touches
//! `org.freedesktop.Notifications`, so the entire zbus layer — the
//! `Notify` payload, the signal subscription, the id ↔ ULID map, the
//! action-key routing — had only manual coverage on a developer's
//! desktop. `plans/LinuxApp.md` §6g asks for this gap to be closed,
//! and notes it would be better coverage than macOS has.
//!
//! The harness is a fake notification server rather than a real one
//! (`dunst`) for three reasons, in increasing order of importance:
//! `dunst` is an X11 client and would drag a virtual display into
//! CI; `dunstctl` can trigger a notification's *default* action but
//! not an arbitrary named one, so `approve` / `reject` would stay
//! unreachable; and a fake can deterministically produce states a
//! real server will not, such as advertising no `actions` capability.
//! What a real server would additionally prove — that our payload is
//! accepted by something we did not write — is worth having, but not
//! at the cost of the paths that actually carry a security decision.
//!
//! Each test owns a private `dbus-daemon`, so these neither see nor
//! disturb the developer's session bus, and they can run in parallel
//! with each other.
//!
//! **Skipping:** when `dbus-daemon` is absent these tests skip rather
//! than fail — a contributor without it should not see red. But they
//! *fail* when `$CI` is set, because a CI run that silently stops
//! exercising this would be worse than no test at all.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vetter_core::wire::WireDecision;
use vetterd::AuditEntry;
use zbus::blocking::connection;
use zbus::zvariant::OwnedValue;

mod common;
use common::{curl_get, make_req, round_trip, Daemon};

const ALLOWLIST: &str = "rules: []\ndeny: []\n";

/// Bus name, object path and interface are all fixed by the
/// freedesktop notification spec; the daemon's proxy is built against
/// these exact strings, so the fake must serve them verbatim.
const NOTIFY_NAME: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";

/// How long to wait for an asynchronous step (a banner to appear, a
/// decision to come back). Generous: these cross a process boundary
/// and a bus, and CI runners are slow and noisy. Polling means the
/// happy path still returns immediately, so a large cap costs nothing
/// when things work and only bounds the failure case.
const SETTLE: Duration = Duration::from_secs(10);

// ── Fake notification server ────────────────────────────────────────────────

/// One `Notify` call as the server received it.
#[derive(Clone, Debug)]
struct Posted {
    id: u32,
    summary: String,
    body: String,
    /// Flat `[key, label, key, label, …]` exactly as it arrived, so a
    /// test can assert on the spec's shape rather than on our
    /// interpretation of it.
    actions: Vec<String>,
    hints: Vec<String>,
    expire_timeout: i32,
}

#[derive(Default)]
struct FakeState {
    next_id: u32,
    posted: Vec<Posted>,
    closed: Vec<u32>,
    capabilities: Vec<String>,
}

struct FakeNotifications {
    state: Arc<Mutex<FakeState>>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl FakeNotifications {
    fn get_capabilities(&self) -> Vec<String> {
        self.state.lock().unwrap().capabilities.clone()
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "vetter-test-fake".into(),
            "vetter".into(),
            "0".into(),
            "1.2".into(),
        )
    }

    /// The spec's `Notify`. Argument order and types are load-bearing:
    /// zbus dispatches on the D-Bus signature, so a mismatch here
    /// surfaces as the daemon's call failing rather than as a
    /// compile error.
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let mut st = self.state.lock().unwrap();
        st.next_id += 1;
        let id = st.next_id;
        let mut hint_keys: Vec<String> = hints.keys().cloned().collect();
        hint_keys.sort();
        st.posted.push(Posted {
            id,
            summary,
            body,
            actions,
            hints: hint_keys,
            expire_timeout,
        });
        id
    }

    fn close_notification(&self, id: u32) {
        self.state.lock().unwrap().closed.push(id);
    }
}

// ── Harness ─────────────────────────────────────────────────────────────────

/// A private session bus, torn down with the struct.
struct PrivateBus {
    address: String,
    pid: u32,
}

impl PrivateBus {
    /// `None` when `dbus-daemon` is not installed — see the skip
    /// policy in the module docs.
    fn start() -> Option<Self> {
        // `--print-address=1 --print-pid=2` splits the two values
        // across stdout and stderr so neither needs parsing out of a
        // combined stream. `--fork` daemonises, so the child we spawn
        // exits immediately and the bus is reparented; that is why the
        // pid has to come from the bus itself rather than from
        // `Child::id`.
        let out = Command::new("dbus-daemon")
            .args(["--session", "--print-address=1", "--print-pid=2", "--fork"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let address = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let pid: u32 = String::from_utf8_lossy(&out.stderr).trim().parse().ok()?;
        if address.is_empty() {
            return None;
        }
        Some(Self { address, pid })
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        // SIGTERM the bus. Its clients (our fake server, and the
        // daemon if it somehow outlived its own guard) drop with it.
        unsafe { libc_kill(self.pid as i32, 15) };
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// Bus + fake server + daemon, wired together and ready to drive.
struct Fixture {
    _bus: PrivateBus,
    conn: zbus::blocking::Connection,
    state: Arc<Mutex<FakeState>>,
    daemon: Daemon,
}

impl Fixture {
    /// `capabilities` is what the fake will answer `GetCapabilities`
    /// with — the knob that lets a test exercise the degraded path
    /// where a server renders no action buttons (§5.4).
    ///
    /// Returns `None` when there is no `dbus-daemon` to build on.
    fn start(capabilities: &[&str]) -> Option<Self> {
        let bus = PrivateBus::start()?;

        let state = Arc::new(Mutex::new(FakeState {
            capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
            ..FakeState::default()
        }));

        // Own the name *before* the daemon starts. Two reasons: the
        // daemon's proxy resolves the owner when it is built, and an
        // unowned-but-activatable name makes D-Bus attempt service
        // activation — which is the ~60 s startup stall fixed in
        // 2026-09-09's `85f8d5f`, and not what these tests are for.
        let conn = connection::Builder::address(bus.address.as_str())
            .expect("bus address")
            .name(NOTIFY_NAME)
            .expect("request notification name")
            .serve_at(
                NOTIFY_PATH,
                FakeNotifications {
                    state: Arc::clone(&state),
                },
            )
            .expect("serve notifications interface")
            .build()
            .expect("connect fake server to private bus");

        let daemon = Daemon::spawn_with_env(
            ALLOWLIST,
            &[
                // Override the harness's default `mock`: this is the
                // one test file that wants the real thing.
                ("VETTERD_NOTIFIER", "linux"),
                ("DBUS_SESSION_BUS_ADDRESS", bus.address.as_str()),
            ],
        );

        Some(Self {
            _bus: bus,
            conn,
            state,
            daemon,
        })
    }

    /// Block until the fake has received a `Notify`, and return it.
    fn wait_for_banner(&self) -> Posted {
        poll_until(|| self.state.lock().unwrap().posted.first().cloned())
            .expect("no Notify reached the notification server")
    }

    /// Deliver `ActionInvoked(id, key)` as a real server would when
    /// the user presses a button.
    fn invoke_action(&self, id: u32, key: &str) {
        self.conn
            .emit_signal(
                Option::<&str>::None,
                NOTIFY_PATH,
                NOTIFY_NAME,
                "ActionInvoked",
                &(id, key),
            )
            .expect("emit ActionInvoked");
    }

    fn audit_entry_for(&self, id: &str) -> AuditEntry {
        let body = poll_until(|| {
            let body = std::fs::read_to_string(&self.daemon.audit).ok()?;
            body.lines().any(|l| l.contains(id)).then_some(body)
        })
        .unwrap_or_else(|| panic!("no audit line for {id}"));
        let line = body.lines().find(|l| l.contains(id)).unwrap();
        serde_json::from_str(line).expect("parse audit entry")
    }
}

/// Poll `f` until it yields a value or [`SETTLE`] elapses.
fn poll_until<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + SETTLE;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Start a fixture, or bail out of the test.
///
/// Skips when `dbus-daemon` is missing so a contributor without it
/// does not see a spurious failure — but fails under `$CI`, where a
/// silently-skipped end-to-end test is indistinguishable from one
/// that never ran.
macro_rules! fixture_or_skip {
    ($caps:expr) => {
        match Fixture::start($caps) {
            Some(f) => f,
            None => {
                if std::env::var_os("CI").is_some() {
                    panic!(
                        "dbus-daemon is unavailable, so the LinuxNotifier end-to-end \
                         smoke cannot run. In CI this is a failure, not a skip: install \
                         it with `tools/install-deps.sh --run --test`."
                    );
                }
                eprintln!("skipping: dbus-daemon not available");
                return;
            }
        }
    };
}

/// Park a request on a background thread and hand back a receiver for
/// its decision.
///
/// `round_trip` blocks until the daemon answers, which is precisely
/// what a parked request does — so the test has to drive the "user"
/// from another thread. The decision comes back over a channel rather
/// than through `JoinHandle::join` so a **broken resolve path fails
/// the test instead of hanging it**: cargo applies no per-test
/// timeout, so a `join` on a request that never resolves would wedge
/// the whole run until CI's job timeout hours later.
fn park_url(
    socket: &std::path::Path,
    url: &str,
) -> (String, mpsc::Receiver<vetter_core::wire::VetDecision>) {
    let req = make_req(curl_get(url), false);
    let id = req.id.clone();
    let socket = socket.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(round_trip(&socket, &req));
    });
    (id, rx)
}

/// Await a parked request's decision, failing rather than hanging.
fn decision_of(
    rx: &mpsc::Receiver<vetter_core::wire::VetDecision>,
) -> vetter_core::wire::VetDecision {
    rx.recv_timeout(SETTLE)
        .expect("parked request was never resolved")
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// The whole point: a request parks, a notification action resolves
/// it, and the audit log attributes the decision to the notification.
#[test]
fn approve_action_resolves_the_parked_request() {
    let fx = fixture_or_skip!(&["actions", "body"]);
    let (id, rx) = park_url(&fx.daemon.socket, "https://approve.example/v1/thing");

    let banner = fx.wait_for_banner();
    fx.invoke_action(banner.id, "approve");

    let decision = decision_of(&rx);
    assert_eq!(decision.decision, WireDecision::Allow);
    assert!(
        decision.reason.contains("notification"),
        "reason should name the surface: {}",
        decision.reason
    );

    let entry = fx.audit_entry_for(&id);
    assert_eq!(entry.decision, WireDecision::Allow);
    assert_eq!(entry.reason, "approved via notification");
}

/// The deny direction, and the reason string §7 step 13 expects to
/// find interleaved with the other surfaces' reasons in the log.
#[test]
fn reject_action_resolves_the_parked_request() {
    let fx = fixture_or_skip!(&["actions", "body"]);
    let (id, rx) = park_url(&fx.daemon.socket, "https://reject.example/v1/thing");

    let banner = fx.wait_for_banner();
    fx.invoke_action(banner.id, "reject");

    assert_eq!(decision_of(&rx).decision, WireDecision::Deny);

    let entry = fx.audit_entry_for(&id);
    assert_eq!(entry.decision, WireDecision::Deny);
    assert_eq!(entry.reason, "rejected via notification");
}

/// An unrecognised action key must not resolve anything. Servers can
/// deliver keys we never registered, and a stray `ActionInvoked` that
/// silently approved a request would be the worst possible bug on
/// this surface.
#[test]
fn unknown_action_key_does_not_resolve() {
    let fx = fixture_or_skip!(&["actions", "body"]);
    let (_id, rx) = park_url(&fx.daemon.socket, "https://unknown-key.example/v1/thing");

    let banner = fx.wait_for_banner();
    fx.invoke_action(banner.id, "definitely-not-one-of-ours");

    // Still parked: nothing came back.
    assert!(
        rx.recv_timeout(Duration::from_secs(1)).is_err(),
        "an unregistered action key must never resolve a request"
    );

    // And the request is still resolvable afterwards, so the stray
    // key did not corrupt the id map either.
    fx.invoke_action(banner.id, "reject");
    assert_eq!(decision_of(&rx).decision, WireDecision::Deny);
}

/// The payload a real server receives. These are correctness
/// properties, not cosmetics: a banner that expires takes an
/// unapprovable request with it, and an action array missing a key
/// means that button is never delivered.
#[test]
fn notify_payload_carries_the_actions_and_never_expires() {
    let fx = fixture_or_skip!(&["actions", "body"]);
    let (_id, rx) = park_url(&fx.daemon.socket, "https://payload.example/v1/thing");

    let banner = fx.wait_for_banner();

    // 0 means "never expire". A prompt that vanishes on a timer is
    // the unapprovable-request failure §0 exists to prevent.
    assert_eq!(
        banner.expire_timeout, 0,
        "prompt banners must not auto-expire"
    );

    // Flat [key, label, …]; assert on keys, since labels are prose.
    let keys: Vec<&str> = banner
        .actions
        .iter()
        .step_by(2)
        .map(String::as_str)
        .collect();
    for want in ["default", "approve", "reject"] {
        assert!(keys.contains(&want), "missing `{want}` in {keys:?}");
    }

    // The unknown host earns the picker shortcut (§6i).
    assert!(
        keys.contains(&"trust_host"),
        "unknown host should offer trust_host: {keys:?}"
    );

    assert!(
        banner.hints.iter().any(|h| h == "urgency"),
        "{:?}",
        banner.hints
    );
    assert!(banner.summary.contains("curl"), "{}", banner.summary);
    assert!(banner.body.contains("payload.example"), "{}", banner.body);

    // Unblock the parked client so the daemon shuts down cleanly.
    fx.invoke_action(banner.id, "reject");
    decision_of(&rx);
}

/// Resolving must clear the banner. The notifier drives this off the
/// queue's change listener rather than off whichever code path
/// resolved the request, so this is the test that the listener is
/// actually wired up.
#[test]
fn resolving_closes_the_banner() {
    let fx = fixture_or_skip!(&["actions", "body"]);
    let (_id, rx) = park_url(&fx.daemon.socket, "https://elsewhere.example/v1/thing");

    let banner = fx.wait_for_banner();
    fx.invoke_action(banner.id, "approve");
    decision_of(&rx);

    let closed = poll_until(|| {
        let st = fx.state.lock().unwrap();
        st.closed.contains(&banner.id).then_some(())
    });
    assert!(
        closed.is_some(),
        "CloseNotification was never called for banner {}",
        banner.id
    );
}

/// A server that advertises no `actions` renders no buttons, so the
/// daemon must still come up and still park requests — the user
/// resolves them from the tray, the window, or `vet daemon approve`
/// (§5.4). Degrading rather than failing is the property under test.
#[test]
fn server_without_actions_capability_still_parks_requests() {
    let fx = fixture_or_skip!(&["body"]);
    let (_id, rx) = park_url(&fx.daemon.socket, "https://degraded.example/v1/thing");

    // A banner still goes out; it simply carries no buttons the
    // server will draw.
    let banner = fx.wait_for_banner();

    // The request is genuinely parked, not silently resolved.
    assert!(
        rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "request should still be pending with no action buttons available"
    );

    // And it is still resolvable — the id map works regardless of
    // whether the server would have rendered the button.
    fx.invoke_action(banner.id, "approve");
    assert_eq!(decision_of(&rx).decision, WireDecision::Allow);
}
