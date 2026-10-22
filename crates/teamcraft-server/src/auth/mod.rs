//! OIDC access-token validation (generic OIDC, Keycloak is the reference; ARCHITECTURE.md §5.1.1).

mod error;
mod extractor;
mod jwks;
mod roles;
mod validator;

#[cfg(any(test, feature = "integration"))]
pub mod test_keys;

pub use error::AuthError;
pub use extractor::AuthenticatedUser;
pub use jwks::{BoxFuture, HttpJwkSource, JwkSource, JwksCache, JwksCacheOptions};
pub use roles::RoleMapping;
pub use validator::{TokenIdentity, TokenValidator, ValidatorOptions};
