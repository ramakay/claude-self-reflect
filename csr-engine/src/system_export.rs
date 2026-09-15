//! Compile-time boundary for future exports into System-tier instruction files.
//!
//! No production constructor exists for [`PlatformConfirmationReceipt`]. A
//! future platform event integration must add that constructor deliberately;
//! user-authored history or ordinary memory metadata cannot mint one.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// The exact bytes and destination proposed for a System-tier export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemExportPayload {
    target_file: String,
    scope: String,
    content: String,
    digest: String,
}

#[derive(Serialize)]
struct CanonicalSystemExportPayload<'a> {
    target_file: &'a str,
    scope: &'a str,
    content: &'a str,
}

impl SystemExportPayload {
    pub fn new(
        target_file: impl Into<String>,
        scope: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let target_file = target_file.into();
        let scope = scope.into();
        let content = content.into();
        let canonical = serde_json::to_string(&CanonicalSystemExportPayload {
            target_file: &target_file,
            scope: &scope,
            content: &content,
        })
        .expect("system export payload serialization is infallible");
        let digest_bytes = Sha256::digest(canonical.as_bytes());
        let mut digest = String::with_capacity(digest_bytes.len() * 2);
        for byte in digest_bytes {
            write!(&mut digest, "{byte:02x}").expect("writing to String is infallible");
        }
        Self {
            target_file,
            scope,
            content,
            digest,
        }
    }
}

/// Opaque proof that the platform recorded confirmation of one exact export.
///
/// Its fields are private and it has no production constructor. This prevents
/// automatic memory distillation—including user-authored history—from being
/// represented as an authorized System-tier export.
#[derive(Debug, PartialEq, Eq)]
pub struct PlatformConfirmationReceipt {
    target_file: String,
    scope: String,
    payload_digest: String,
    _platform_only: PlatformOnly,
}

#[derive(Debug, PartialEq, Eq)]
struct PlatformOnly;

impl PlatformConfirmationReceipt {
    #[cfg(test)]
    fn for_test(payload: &SystemExportPayload) -> Self {
        Self {
            target_file: payload.target_file.clone(),
            scope: payload.scope.clone(),
            payload_digest: payload.digest.clone(),
            _platform_only: PlatformOnly,
        }
    }
}

/// An export request whose exact payload has a matching platform receipt.
/// Future writers must accept this type rather than an unconfirmed payload.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub struct SystemExportRequest {
    payload: SystemExportPayload,
    _confirmation: PlatformConfirmationReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemExportAuthorizationError {
    PayloadMismatch,
}

impl SystemExportRequest {
    pub fn authorize(
        payload: SystemExportPayload,
        confirmation: PlatformConfirmationReceipt,
    ) -> Result<Self, SystemExportAuthorizationError> {
        let exact_match = confirmation.target_file == payload.target_file
            && confirmation.scope == payload.scope
            && confirmation.payload_digest == payload.digest;
        if !exact_match {
            return Err(SystemExportAuthorizationError::PayloadMismatch);
        }
        Ok(Self {
            payload,
            _confirmation: confirmation,
        })
    }

    pub fn target_file(&self) -> &str {
        &self.payload.target_file
    }

    pub fn scope(&self) -> &str {
        &self.payload.scope
    }

    pub fn content(&self) -> &str {
        &self.payload.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_export_requires_an_exact_platform_confirmed_payload() {
        let payload = SystemExportPayload::new(
            "/repo/CLAUDE.md",
            "project instructions",
            "Never publish without approval.",
        );
        let receipt = PlatformConfirmationReceipt::for_test(&payload);
        let request = SystemExportRequest::authorize(payload.clone(), receipt)
            .expect("an exact platform receipt authorizes only this payload");
        assert_eq!(request.target_file(), "/repo/CLAUDE.md");
        assert_eq!(request.scope(), "project instructions");
        assert_eq!(request.content(), "Never publish without approval.");

        for mismatched in [
            SystemExportPayload::new(
                "/repo/OTHER.md",
                "project instructions",
                "Never publish without approval.",
            ),
            SystemExportPayload::new(
                "/repo/CLAUDE.md",
                "global instructions",
                "Never publish without approval.",
            ),
            SystemExportPayload::new(
                "/repo/CLAUDE.md",
                "project instructions",
                "Publish automatically.",
            ),
        ] {
            let receipt = PlatformConfirmationReceipt::for_test(&payload);
            assert_eq!(
                SystemExportRequest::authorize(mismatched, receipt),
                Err(SystemExportAuthorizationError::PayloadMismatch)
            );
        }
    }
}
