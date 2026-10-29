//! Live-network verification for the Grok assistant transport.
//!
//! Deliberately `#[ignore]`d: these tests call the real `api.x.ai` endpoint
//! with a fake key and are only meaningful when run explicitly
//! (`cargo test --test grok_live -- --ignored`). They never use real
//! credentials and never spend tokens — the models list is free, so a
//! well-formed but unauthorized request must come back classified as an
//! auth error, proving TLS, host pinning, the Bearer header, and the
//! error classifier against the real provider.

use bzdium_lib::grok::{self, GrokErrorKind};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bzdium-grok-live-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
#[ignore = "hits the real api.x.ai with a fake key; run explicitly"]
fn live_models_endpoint_rejects_a_bad_key_as_auth() {
    let dir = temp_dir("badkey");
    // Deliberately fake, unusable credential literal.
    grok::save_api_key(&dir, "xai-TESTKEY-not-a-real-credential-000000").unwrap();

    let err = grok::fetch_models(&dir).expect_err("a fake key must not authenticate");
    assert_eq!(
        err.kind,
        GrokErrorKind::Auth,
        "expected the provider's 401 to classify as auth, got: {err:?}"
    );
    // The sanitized message must not echo the fake key.
    assert!(!err.message.contains("TESTKEY"), "error leaked the key: {}", err.message);

    grok::clear_api_key(&dir);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore = "hits the real api.x.ai; requires no key and proves the unauthenticated path"]
fn live_models_endpoint_without_key_is_auth_not_network() {
    let dir = temp_dir("nokey");
    let err = grok::fetch_models(&dir).expect_err("no key configured");
    assert_eq!(err.kind, GrokErrorKind::Auth);
    std::fs::remove_dir_all(&dir).ok();
}
