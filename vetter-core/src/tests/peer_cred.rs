//! Tests for [`crate::peer_cred`]. Layout convention is described in
//! `AGENTS.md`.

use std::os::unix::net::UnixStream;

use super::*;

#[test]
fn peer_uid_of_socketpair_is_current_euid() {
    let (a, _b) = UnixStream::pair().expect("socketpair");
    let me = current_euid();
    let peer = peer_uid(&a).expect("getpeereid on connected pair");
    assert_eq!(
        peer, me,
        "socketpair within one process must report self uid"
    );
}

#[test]
fn assert_peer_is_self_passes_for_socketpair() {
    let (a, _b) = UnixStream::pair().expect("socketpair");
    assert_peer_is_self(&a).expect("same-uid pair must pass");
}
