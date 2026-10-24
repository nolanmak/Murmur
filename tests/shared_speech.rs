//! Run producer-owned contracts against the dependency this app resolves.
#[test]
fn shared_capture_formats_and_normalization() {
    fotw_audio::conformance::formats();
    fotw_stt::conformance::normalization();
}

#[tokio::test]
async fn shared_local_lifecycle() {
    fotw_stt::conformance::local_lifecycle().await;
}
