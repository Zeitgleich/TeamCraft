//! OIDC access-token validation (generic OIDC, Keycloak is the reference; ARCHITECTURE.md §5.1.1).

mod dpop;
mod error;
mod extractor;
mod jwks;
mod revocations;
mod roles;
mod validator;

#[cfg(any(test, feature = "integration"))]
pub mod test_keys;

pub use dpop::{
  DPOP_HEADER, DPOP_NONCE_HEADER, DpopGuard, DpopRefusal, PresentedProof, PresentedToken, TokenBinding,
  TokenScheme, enforce as enforce_dpop, note_nonce_header, public_origins,
};
pub use error::AuthError;
pub use extractor::{AdminUser, AuthenticatedUser, InboxCaller, SfuCaller};
pub use jwks::{BoxFuture, HttpJwkSource, JwkSource, JwksCache, JwksCacheOptions};
pub use revocations::Revocations;
pub use roles::RoleMapping;
pub use validator::{LogoutIdentity, TokenIdentity, TokenValidator, ValidatorOptions};
