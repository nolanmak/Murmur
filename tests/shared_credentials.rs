//! Same credential contract used by the producer, against our pinned dependency.
#[test]
fn shared_credentials_preserve_addressing_isolation_and_redaction() {
    fotw_secrets::conformance::credentials();
}
