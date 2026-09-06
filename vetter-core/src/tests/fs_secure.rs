//! Tests for [`crate::fs_secure`]. Layout convention from `AGENTS.md`.

use super::*;

#[test]
fn create_dir_secure_chmods_leaf_to_mode() {
    let dir = tempfile::tempdir().unwrap();
    let leaf = dir.path().join("a/b/c");
    create_dir_secure(&leaf, 0o700).unwrap();
    let mode = std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "leaf should be 0700, got 0{mode:o}");
}

#[test]
fn create_dir_secure_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let leaf = dir.path().join("nested");
    create_dir_secure(&leaf, 0o700).unwrap();
    // Loosen, then re-call: should re-tighten.
    std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o755)).unwrap();
    create_dir_secure(&leaf, 0o700).unwrap();
    let mode = std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

#[test]
fn create_dir_secure_does_not_chmod_intermediate_components() {
    // We deliberately tighten only the leaf — see module docs.
    let dir = tempfile::tempdir().unwrap();
    let intermediate = dir.path().join("intermediate");
    let leaf = intermediate.join("leaf");
    create_dir_secure(&leaf, 0o700).unwrap();
    let leaf_mode = std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777;
    assert_eq!(leaf_mode, 0o700);
    // Intermediate may be whatever the umask produced; just assert
    // we didn't accidentally tighten it to 0700.
    let inter_mode = std::fs::metadata(&intermediate)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_ne!(
        inter_mode, 0o700,
        "intermediate dir should keep umask default, got 0{inter_mode:o}"
    );
}

#[test]
fn persist_at_mode_lands_file_at_mode() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.yaml");
    let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
    tmp.write_all(b"contents").unwrap();
    persist_at_mode(tmp, &dest, 0o600).unwrap();
    assert!(dest.exists());
    let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "dest should be 0600, got 0{mode:o}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"contents");
}

#[test]
fn persist_at_mode_overwrites_existing_at_target_mode() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.yaml");
    // Pre-existing file at a wider mode.
    std::fs::write(&dest, "old").unwrap();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).unwrap();

    let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
    tmp.write_all(b"new").unwrap();
    persist_at_mode(tmp, &dest, 0o600).unwrap();
    let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "persist must replace the existing file's mode, got 0{mode:o}"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"new");
}
