use super::*;
use ed25519_dalek::{Signer, SigningKey};

fn fixture(version: &str) -> (String, String, String) {
    let key = SigningKey::from_bytes(&[42; 32]);
    let m = Manifest {
        version: version.into(),
        notes: "release".into(),
        target: "windows-x86_64".into(),
        url: format!("https://github.com/{REPOSITORY}/releases/download/v{version}/BetterWX.exe"),
        size: 1024,
        sha256: "a".repeat(64),
    };
    let document = serde_json::to_string(&m).unwrap();
    let signature = STANDARD.encode(key.sign(document.as_bytes()).to_bytes());
    (
        document,
        signature,
        STANDARD.encode(key.verifying_key().to_bytes()),
    )
}

#[test]
fn accepts_signed_stable_release() {
    let (d, s, k) = fixture("2.3.0");
    assert_eq!(
        verify(d.as_bytes(), &s, &k, "2.2.0").unwrap().version,
        "2.3.0"
    );
}
#[test]
fn rejects_tampering_wrong_key_downgrade_and_prerelease() {
    let (d, s, k) = fixture("2.3.0");
    assert!(verify(d.replace("release", "changed").as_bytes(), &s, &k, "2.2.0").is_err());
    let other = STANDARD.encode(SigningKey::from_bytes(&[43; 32]).verifying_key().to_bytes());
    assert!(verify(d.as_bytes(), &s, &other, "2.2.0").is_err());
    assert!(verify(d.as_bytes(), &s, &k, "2.3.0").is_err());
    assert!(verify(d.as_bytes(), &s, &k, "3.0.0").is_err());
    let (d, s, k) = fixture("2.4.0-beta.1");
    assert!(verify(d.as_bytes(), &s, &k, "2.2.0").is_err());
}
#[test]
fn rejects_signed_foreign_asset_and_architecture() {
    let (d, _, k) = fixture("2.3.0");
    let key = SigningKey::from_bytes(&[42; 32]);
    for d in [
        d.replace("github.com", "evil.example"),
        d.replace("windows-x86_64", "windows-arm64"),
        d.replace("/v2.3.0/", "/v2.1.0/"),
    ] {
        let s = STANDARD.encode(key.sign(d.as_bytes()).to_bytes());
        assert!(verify(d.as_bytes(), &s, &k, "2.2.0").is_err());
    }
}

#[test]
fn failed_stage_preserves_original_executable() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("app.exe");
    fs::write(&target, b"original").unwrap();
    assert!(
        replace_and_launch(
            &target,
            &dir.path().join("missing.exe"),
            &dir.path().join("ready")
        )
        .is_err()
    );
    assert_eq!(fs::read(target).unwrap(), b"original");
}

#[test]
fn invalid_new_executable_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("app.exe");
    let package = dir.path().join("new.exe");
    fs::write(&target, b"old-invalid-test-executable").unwrap();
    fs::write(&package, b"new-invalid-test-executable").unwrap();
    assert!(replace_and_launch(&target, &package, &dir.path().join("ready")).is_err());
    assert_eq!(fs::read(target).unwrap(), b"old-invalid-test-executable");
}
