//! Key validation (KEY-08): "is this key real?", asked cheaply.
//!
//! Each provider gets one cheap, read-only, non-billable endpoint and the auth
//! header form it actually documents. Getting the header form wrong produces a
//! 401 that is indistinguishable from a bad key, which sends the user hunting
//! for a typo that does not exist — so the forms are pinned by test.
//!
//! # Correction to the brief: Anthropic uses `x-api-key`
//!
//! The task brief specified `Authorization: Bearer …` for **both** OpenAI and
//! Anthropic. That is right for OpenAI and wrong for Anthropic: the Anthropic
//! API authenticates API keys with an `x-api-key` header plus a required
//! `anthropic-version`. `Authorization: Bearer` on that API carries an *OAuth*
//! token, a different credential type we do not use, and additionally requires
//! its own beta header.
//!
//! docs/REQUIREMENTS.md 10 corroborates the correction from inside the tree:
//! its never-log list is `Authorization`, `xi-api-key`, `Token`, `x-api-key` —
//! `x-api-key` is on that list precisely because Anthropic sends keys in it.
//! We implement the header that works.
//!
//! # This crate opens no sockets
//!
//! [`ValidationClient`] is a trait with no built-in implementation, and
//! `fotw-secrets` has no HTTP dependency. That is deliberate on two counts.
//! The workspace already decided that provider sockets live in `fotw-stt`
//! (see the root `Cargo.toml`), and the egress allowlist and privacy-flag
//! injection of KEY-02/KEY-03 belong in the one HTTP wrapper that enforces
//! them — a second, independent HTTP path inside the secrets crate would be a
//! way around both. It also means these tests *cannot* accidentally make a
//! network call: there is nothing here that could.

use std::fmt;

use crate::{Provider, SecretString};

/// A validation request: everything the transport needs except the key.
///
/// The key is not a field. The request says which header carries it and what
/// prefix it takes; the transport materialises the value at send time via
/// [`ValidationRequest::auth_header_value`]. So this struct can be logged,
/// stored or `Debug`-printed with no possibility of leaking a credential —
/// there is none in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationRequest {
    /// HTTP method. Always `GET`: validation must never mutate.
    pub method: &'static str,
    /// Absolute URL of the validation endpoint.
    pub url: String,
    /// Where the key goes and how it is framed.
    pub auth: AuthScheme,
    /// Non-credential headers the provider requires.
    pub extra_headers: Vec<(&'static str, &'static str)>,
}

impl ValidationRequest {
    /// The header the key travels in.
    #[must_use]
    pub fn auth_header_name(&self) -> &'static str {
        self.auth.header
    }

    /// Materialise the credential header value.
    ///
    /// Returns a [`SecretString`], not a `String`: the framed value
    /// (`Token dg-…`) is every bit as sensitive as the raw key, and callers
    /// that hold it should get the same redaction and zeroing. This is the
    /// only `expose()` on the validation path.
    #[must_use]
    pub fn auth_header_value(&self, key: &SecretString) -> SecretString {
        if self.auth.prefix.is_empty() {
            SecretString::new(key.expose())
        } else {
            SecretString::new(format!("{}{}", self.auth.prefix, key.expose()))
        }
    }

    /// The host this request targets.
    ///
    /// Used by the disclosure screen (KEY-04), which must name the endpoint
    /// host before the first request to a newly configured provider, and by
    /// the egress allowlist test.
    #[must_use]
    pub fn host(&self) -> &str {
        self.url
            .trim_start_matches("https://")
            .split('/')
            .next()
            .unwrap_or_default()
    }
}

/// Where a provider expects the key, and how it is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthScheme {
    /// Header name, e.g. `Authorization` or `x-api-key`.
    pub header: &'static str,
    /// Text placed before the key, e.g. `"Token "`, `"Bearer "`, or `""`.
    pub prefix: &'static str,
}

/// A response, reduced to what validation actually reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body, for diagnostics. Provider error bodies do not echo the
    /// key; if one ever did, [`crate::Redactor`] is the backstop.
    pub body: String,
}

/// The request never reached a server.
///
/// Kept separate from an HTTP status on purpose — this distinction is the
/// whole point of [`ValidationOutcome::NetworkBlocked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    /// What went wrong, for the user: DNS failure, proxy rejection, TLS error.
    pub detail: String,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

/// Something that can perform a validation request.
///
/// Injected so validation can be tested without a network, and so the one
/// place that owns egress policy also owns the socket.
pub trait ValidationClient {
    /// Perform `request`, sending `key` in the header the request names.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when the request did not reach the server. An HTTP
    /// error status is a success here — it is a response, and the status is
    /// what validation interprets.
    fn send(
        &self,
        request: &ValidationRequest,
        key: &SecretString,
    ) -> Result<HttpResponse, TransportError>;
}

/// What we learned about a key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValidationOutcome {
    /// The provider accepted the key.
    Valid,
    /// The provider rejected the key. Conclusive: it is wrong.
    InvalidKey,
    /// The key is real but lacks the scope this endpoint needs.
    ///
    /// Distinct from [`ValidationOutcome::InvalidKey`] because the fix is
    /// different: the user must widen the key's permissions, not re-type it.
    InsufficientScope,
    /// Authentication succeeded; the account is being throttled.
    RateLimited,
    /// The provider answered with something unhelpful — usually an outage.
    ProviderError {
        /// The HTTP status returned.
        status: u16,
    },
    /// **We never reached the provider.**
    ///
    /// A corporate proxy, a captive portal, DNS filtering, or a laptop that is
    /// simply offline. This must never be collapsed into
    /// [`ValidationOutcome::InvalidKey`]: a user told their key is invalid
    /// will delete a working key and go generate another one, which will fail
    /// in exactly the same way.
    NetworkBlocked {
        /// What the transport reported, so the user can see it is a network
        /// problem rather than a key problem.
        detail: String,
    },
}

impl ValidationOutcome {
    /// Whether the key can be used for real work.
    ///
    /// True for [`Valid`](ValidationOutcome::Valid) and
    /// [`RateLimited`](ValidationOutcome::RateLimited) — a throttled key
    /// authenticated successfully, so it is a good key on a busy account.
    #[must_use]
    pub fn key_is_usable(&self) -> bool {
        matches!(self, Self::Valid | Self::RateLimited)
    }

    /// Whether we failed to learn anything about the key.
    ///
    /// The UI must not prompt for re-entry when this is true — there is no
    /// evidence against the key, and asking is how a user is talked into
    /// discarding a working credential.
    #[must_use]
    pub fn is_inconclusive(&self) -> bool {
        matches!(
            self,
            Self::NetworkBlocked { .. } | Self::ProviderError { .. }
        )
    }

    /// One line of copy for the settings screen.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Valid => "Key verified.".to_owned(),
            Self::InvalidKey => {
                "This provider rejected the key. Check it and try again.".to_owned()
            }
            Self::InsufficientScope => {
                "The key is valid but lacks the permissions this needs. Widen its scope in the \
                 provider's dashboard."
                    .to_owned()
            }
            Self::RateLimited => "Key verified. The account is rate limited right now.".to_owned(),
            Self::ProviderError { status } => {
                format!("The provider returned an error ({status}). The key was not checked.")
            }
            Self::NetworkBlocked { detail } => {
                format!("Could not reach the provider, so the key was not checked: {detail}")
            }
        }
    }
}

/// The validation request for a provider.
///
/// Endpoints are chosen to be cheap, read-only, and non-billable — a
/// "check my key" button that costs money gets pressed once.
#[must_use]
pub fn request_for(provider: Provider) -> ValidationRequest {
    match provider {
        // `GET /v1/projects` lists the key's projects: no audio, no charge.
        Provider::Deepgram => ValidationRequest {
            method: "GET",
            url: "https://api.deepgram.com/v1/projects".to_owned(),
            auth: AuthScheme {
                header: "Authorization",
                prefix: "Token ",
            },
            extra_headers: Vec::new(),
        },
        // `GET /v1/user` returns the subscription the key belongs to.
        Provider::ElevenLabs => ValidationRequest {
            method: "GET",
            url: "https://api.elevenlabs.io/v1/user".to_owned(),
            auth: AuthScheme {
                header: "xi-api-key",
                prefix: "",
            },
            extra_headers: Vec::new(),
        },
        // `GET /v1/models` is the canonical free auth check.
        Provider::OpenAi => ValidationRequest {
            method: "GET",
            url: "https://api.openai.com/v1/models".to_owned(),
            auth: AuthScheme {
                header: "Authorization",
                prefix: "Bearer ",
            },
            extra_headers: Vec::new(),
        },
        // `x-api-key`, not `Authorization: Bearer` -- see the module docs for
        // why this departs from the brief. `anthropic-version` is mandatory on
        // every Anthropic API request, including this one.
        Provider::Anthropic => ValidationRequest {
            method: "GET",
            url: "https://api.anthropic.com/v1/models".to_owned(),
            auth: AuthScheme {
                header: "x-api-key",
                prefix: "",
            },
            extra_headers: vec![("anthropic-version", "2023-06-01")],
        },
    }
}

/// Check a key against its provider.
///
/// Never panics and never blocks on anything but `client`.
#[must_use]
pub fn validate(
    provider: Provider,
    key: &SecretString,
    client: &dyn ValidationClient,
) -> ValidationOutcome {
    // Short-circuit before the network. An empty key cannot be valid, and
    // sending one produces a 401 that tells the user their key is wrong when
    // what actually happened is that the field was blank.
    if key.is_empty() {
        return ValidationOutcome::InvalidKey;
    }

    let request = request_for(provider);
    match client.send(&request, key) {
        Ok(response) => classify(response.status),
        // The single most important line in this module: a transport failure
        // is its own state, never a verdict on the key.
        Err(err) => ValidationOutcome::NetworkBlocked { detail: err.detail },
    }
}

/// Map an HTTP status to an outcome.
fn classify(status: u16) -> ValidationOutcome {
    match status {
        200..=299 => ValidationOutcome::Valid,
        // 401 is the only conclusive "this key is wrong".
        401 => ValidationOutcome::InvalidKey,
        // 403 is a real key that is not allowed here.
        403 => ValidationOutcome::InsufficientScope,
        // 429 means we got past authentication.
        429 => ValidationOutcome::RateLimited,
        other => ValidationOutcome::ProviderError { status: other },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::validate::{
        HttpResponse, TransportError, ValidationClient, ValidationOutcome, ValidationRequest,
        request_for, validate,
    };
    use crate::{Provider, SecretString};

    /// Records what was asked of it and replays a scripted answer. The only
    /// [`ValidationClient`] in this crate's tests — there is no real HTTP
    /// client here to accidentally reach for, because `fotw-secrets` has no
    /// HTTP dependency at all.
    struct FakeClient {
        reply: Result<HttpResponse, TransportError>,
        seen: Mutex<Vec<(String, String, String, String)>>,
    }

    impl FakeClient {
        fn responding(status: u16) -> Self {
            Self {
                reply: Ok(HttpResponse {
                    status,
                    body: String::new(),
                }),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn failing(detail: &str) -> Self {
            Self {
                reply: Err(TransportError {
                    detail: detail.to_owned(),
                }),
                seen: Mutex::new(Vec::new()),
            }
        }

        /// (method, url, auth header name, auth header value)
        fn last(&self) -> (String, String, String, String) {
            self.seen
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("no request was made")
        }
    }

    impl ValidationClient for FakeClient {
        fn send(
            &self,
            request: &ValidationRequest,
            key: &SecretString,
        ) -> Result<HttpResponse, TransportError> {
            self.seen.lock().unwrap().push((
                request.method.to_owned(),
                request.url.clone(),
                request.auth_header_name().to_owned(),
                request.auth_header_value(key).expose().to_owned(),
            ));
            match &self.reply {
                Ok(response) => Ok(HttpResponse {
                    status: response.status,
                    body: response.body.clone(),
                }),
                Err(err) => Err(TransportError {
                    detail: err.detail.clone(),
                }),
            }
        }
    }

    // ------------------------------------------------ endpoints and headers

    /// The header *form* per provider. Getting this wrong produces a 401 that
    /// looks exactly like a bad key, so the shape is pinned here rather than
    /// discovered by a user whose correct key is rejected.
    #[test]
    fn each_provider_uses_its_own_documented_auth_header() {
        let key = SecretString::new("KEYMATERIAL");

        let dg = request_for(Provider::Deepgram);
        assert_eq!(dg.auth_header_name(), "Authorization");
        assert_eq!(dg.auth_header_value(&key).expose(), "Token KEYMATERIAL");

        let el = request_for(Provider::ElevenLabs);
        assert_eq!(el.auth_header_name(), "xi-api-key");
        assert_eq!(el.auth_header_value(&key).expose(), "KEYMATERIAL");

        let oa = request_for(Provider::OpenAi);
        assert_eq!(oa.auth_header_name(), "Authorization");
        assert_eq!(oa.auth_header_value(&key).expose(), "Bearer KEYMATERIAL");

        // Anthropic authenticates API keys with `x-api-key`, NOT
        // `Authorization: Bearer` -- see the module docs.
        let an = request_for(Provider::Anthropic);
        assert_eq!(an.auth_header_name(), "x-api-key");
        assert_eq!(an.auth_header_value(&key).expose(), "KEYMATERIAL");
    }

    #[test]
    fn anthropic_sends_the_required_api_version_header() {
        let an = request_for(Provider::Anthropic);
        assert!(
            an.extra_headers
                .iter()
                .any(|(name, value)| *name == "anthropic-version" && !value.is_empty()),
            "Anthropic rejects requests with no anthropic-version header"
        );
    }

    /// The endpoints are cheap, read-only, and on the egress allowlist in
    /// docs/REQUIREMENTS.md 10. A validation call that hit a *billable*
    /// endpoint would make "check my key" cost money.
    #[test]
    fn endpoints_are_cheap_reads_on_allowlisted_hosts() {
        let expected = [
            (
                Provider::Deepgram,
                "api.deepgram.com",
                "https://api.deepgram.com/v1/projects",
            ),
            (
                Provider::ElevenLabs,
                "api.elevenlabs.io",
                "https://api.elevenlabs.io/v1/user",
            ),
            (
                Provider::OpenAi,
                "api.openai.com",
                "https://api.openai.com/v1/models",
            ),
            (
                Provider::Anthropic,
                "api.anthropic.com",
                "https://api.anthropic.com/v1/models",
            ),
        ];

        for (provider, host, url) in expected {
            let request = request_for(provider);
            assert_eq!(
                request.method, "GET",
                "{provider} validation must not mutate"
            );
            assert_eq!(request.url, url);
            assert_eq!(request.host(), host);
        }
    }

    /// The request describes *where* the key goes; it never holds one. So the
    /// struct the transport logs cannot leak a key even if logged raw.
    #[test]
    fn a_validation_request_holds_no_key_material() {
        for provider in Provider::ALL {
            let rendered = format!("{:?}", request_for(provider));
            assert!(
                !rendered.to_lowercase().contains("keymaterial"),
                "{rendered}"
            );
        }
    }

    // ----------------------------------------------------- outcome mapping

    #[test]
    fn a_successful_response_means_the_key_is_valid() {
        let client = FakeClient::responding(200);
        let outcome = validate(Provider::Deepgram, &SecretString::new("dg-good"), &client);

        assert_eq!(outcome, ValidationOutcome::Valid);
        assert!(outcome.key_is_usable());

        let (method, url, header, value) = client.last();
        assert_eq!(method, "GET");
        assert_eq!(url, "https://api.deepgram.com/v1/projects");
        assert_eq!(header, "Authorization");
        assert_eq!(value, "Token dg-good");
    }

    #[test]
    fn a_401_means_the_key_is_wrong() {
        let client = FakeClient::responding(401);
        let outcome = validate(Provider::OpenAi, &SecretString::new("sk-bad"), &client);
        assert_eq!(outcome, ValidationOutcome::InvalidKey);
        assert!(!outcome.key_is_usable());
    }

    /// A 403 is a *real* key without the right scope. Telling the user to
    /// re-type it sends them to look for a typo that is not there.
    #[test]
    fn a_403_is_reported_as_a_scope_problem_not_a_bad_key() {
        let client = FakeClient::responding(403);
        let outcome = validate(Provider::Anthropic, &SecretString::new("real-key"), &client);
        assert_eq!(outcome, ValidationOutcome::InsufficientScope);
        assert_ne!(outcome, ValidationOutcome::InvalidKey);
    }

    /// A 429 authenticated successfully — the key is fine, the account is
    /// busy. Marking it invalid would have the user rotate a working key.
    #[test]
    fn a_429_means_the_key_works_and_the_account_is_throttled() {
        let client = FakeClient::responding(429);
        let outcome = validate(Provider::ElevenLabs, &SecretString::new("el-key"), &client);
        assert_eq!(outcome, ValidationOutcome::RateLimited);
        assert!(
            outcome.key_is_usable(),
            "a throttled key is still a good key"
        );
    }

    #[test]
    fn a_5xx_is_the_provider_being_down_not_the_key_being_bad() {
        let client = FakeClient::responding(503);
        let outcome = validate(Provider::Deepgram, &SecretString::new("dg-key"), &client);
        assert_eq!(outcome, ValidationOutcome::ProviderError { status: 503 });
        assert_ne!(outcome, ValidationOutcome::InvalidKey);
    }

    /// **The requirement.** A corporate proxy, captive portal, or offline
    /// laptop must not be reported as a typo. Someone told "that key is
    /// invalid" will delete a perfectly good key and go get another one.
    #[test]
    fn a_transport_failure_is_network_blocked_and_never_invalid_key() {
        let client = FakeClient::failing("tcp connect: connection refused (proxy 407)");
        let outcome = validate(
            Provider::OpenAi,
            &SecretString::new("sk-perfectly-fine"),
            &client,
        );

        assert_ne!(
            outcome,
            ValidationOutcome::InvalidKey,
            "a blocked network was reported as a bad key"
        );
        match &outcome {
            ValidationOutcome::NetworkBlocked { detail } => {
                assert!(detail.contains("407"), "the user needs the cause: {detail}");
            }
            other => panic!("expected NetworkBlocked, got {other:?}"),
        }
        assert!(!outcome.key_is_usable(), "we could not confirm the key");
        assert!(
            outcome.is_inconclusive(),
            "a blocked network says nothing about the key, either way"
        );
    }

    /// The other half of the same requirement: a *rejection* is conclusive, so
    /// the UI may safely tell the user to re-enter the key.
    #[test]
    fn only_a_blocked_network_is_inconclusive() {
        assert!(!ValidationOutcome::InvalidKey.is_inconclusive());
        assert!(!ValidationOutcome::Valid.is_inconclusive());
        assert!(
            ValidationOutcome::ProviderError { status: 503 }.is_inconclusive(),
            "a provider outage tells us nothing about the key either"
        );
        assert!(
            ValidationOutcome::NetworkBlocked {
                detail: "dns".to_owned()
            }
            .is_inconclusive()
        );
    }

    #[test]
    fn every_outcome_has_user_facing_copy() {
        let outcomes = [
            ValidationOutcome::Valid,
            ValidationOutcome::InvalidKey,
            ValidationOutcome::InsufficientScope,
            ValidationOutcome::RateLimited,
            ValidationOutcome::ProviderError { status: 500 },
            ValidationOutcome::NetworkBlocked {
                detail: "dns failure".to_owned(),
            },
        ];
        for outcome in outcomes {
            assert!(
                !outcome.user_message().is_empty(),
                "{outcome:?} has no copy"
            );
        }
    }

    #[test]
    fn an_empty_key_is_rejected_without_touching_the_network() {
        let client = FakeClient::responding(200);
        let outcome = validate(Provider::Deepgram, &SecretString::new(""), &client);
        assert_eq!(outcome, ValidationOutcome::InvalidKey);
        assert!(
            client.seen.lock().unwrap().is_empty(),
            "sent an empty key to the provider"
        );
    }
}
