use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn hashing_uses_fresh_random_salts() {
    let first = memento::cli::hash_password("synthetic test password").unwrap();
    let second = memento::cli::hash_password("synthetic test password").unwrap();
    assert_ne!(first, second);
}

#[test]
fn init_db_is_explicit_idempotent_and_does_not_start_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.db");
    for _ in 0..2 {
        let out = Command::new(env!("CARGO_BIN_EXE_memento"))
            .args(["init-db", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(!String::from_utf8_lossy(&out.stdout).contains("key"));
    }
    let conn = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM diaries", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn unknown_arguments_fail_instead_of_starting_server() {
    let out = Command::new(env!("CARGO_BIN_EXE_memento"))
        .arg("not-a-command")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn password_hash_accepts_stdin_without_echo_and_verifies() {
    let password = "test-only password 😀";
    let mut child = Command::new(env!("CARGO_BIN_EXE_memento"))
        .args(["hash-password", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{password}\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let hash = String::from_utf8(output.stdout).unwrap();
    assert!(!hash.contains(password));
    assert!(hash.starts_with("$2b$"));
    assert!(bcrypt::verify(password, hash.trim()).unwrap());
    assert!(output.stderr.is_empty());
}

#[test]
fn hash_rejects_empty_and_bcrypt_truncating_input_without_echo() {
    for password in [String::new(), "x".repeat(73), "密".repeat(25)] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_memento"))
            .args(["hash-password", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(password.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        if !password.is_empty() {
            assert!(!String::from_utf8_lossy(&output.stderr).contains(&password));
        }
    }
}

#[test]
fn bcrypt_accepts_short_passwords_and_its_exact_byte_boundary() {
    for password in ["short".to_owned(), "密".repeat(24)] {
        let hash = memento::cli::hash_password(&password).unwrap();
        assert!(bcrypt::verify(&password, &hash).unwrap());
    }
}

#[test]
fn totp_secret_command_generates_unique_160_bit_base32_secrets() {
    let mut previous = None;
    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_memento"))
            .arg("totp-secret")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let encoded = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        let raw = totp_rs::Secret::Encoded(encoded.clone())
            .to_bytes()
            .unwrap();
        assert_eq!(raw.len(), 20);
        assert_ne!(previous.as_ref(), Some(&encoded));
        previous = Some(encoded);
    }
}
