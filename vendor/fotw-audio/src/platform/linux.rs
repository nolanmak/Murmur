//! linux capture backend — not implemented yet.
//!
//! Kept as a compiling placeholder so the seam is proven against this target
//! in CI. Real capture is tracked in the issue tracker; until then the host
//! resolves to [`crate::platform::StubPlatform`].

// Intentionally empty. Adding OS types here is fine — this file is inside
// `platform/`, which is the only place the seam permits them.
