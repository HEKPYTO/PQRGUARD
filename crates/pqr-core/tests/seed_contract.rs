#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pqr_core::seed::Seed;

#[test]
fn debug_redacts_seed_bytes() {
    let seed = Seed::from_bytes([0xAB; 32]);
    let debug = format!("{seed:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("ab"));
}

#[test]
fn parse_requires_exact_length_and_hex() {
    let raw = [7u8; 32];
    assert_eq!(Seed::parse(&raw, false).unwrap().as_bytes(), &raw);
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(Seed::parse(hex.as_bytes(), true).unwrap().as_bytes(), &raw);
    assert_eq!(
        Seed::parse(format!("  {hex}\n").as_bytes(), true)
            .unwrap()
            .as_bytes(),
        &raw
    );
    assert!(Seed::parse(b"short", true).is_err());
    assert!(Seed::parse(&[0u8; 33], false).is_err());
    assert!(Seed::parse(&[b'z'; 64], true).is_err());
}

#[test]
fn seed_file_rejects_symlinks_and_loose_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("seed.bin");
    fs::write(&path, [0x11u8; 32]).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(Seed::from_file(&path).unwrap().as_bytes(), &[0x11u8; 32]);

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Seed::from_file(&path).is_err());

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let link = dir.path().join("link.bin");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(Seed::from_file(&link).is_err());

    let large = dir.path().join("large.bin");
    fs::write(&large, [0u8; 65]).unwrap();
    fs::set_permissions(&large, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Seed::from_file(&large).is_err());
}

#[test]
fn seed_from_env_and_command() {
    let raw = [0x22u8; 32];
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    std::env::set_var("PQR_GUARD_TEST_SEED", hex.as_str());
    assert_eq!(
        Seed::from_env("PQR_GUARD_TEST_SEED").unwrap().as_bytes(),
        &raw
    );
    std::env::remove_var("PQR_GUARD_TEST_SEED");

    let command = format!("printf '%s' {hex}");
    assert_eq!(Seed::from_command(&command).unwrap().as_bytes(), &raw);
    assert!(Seed::from_command("exit 1").is_err());
}
