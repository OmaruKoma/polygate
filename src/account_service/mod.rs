//! Account and credential lifecycle services.
//!
//! Dependency direction is deliberate:
//!
//! ```text
//! AccountService -> Storage -> Security
//!                -> accounts (domain types)
//! ProviderAdapter / Scheduler / CheckinTask -> AccountService
//! ```
//!
//! The domain module `accounts` does not know about storage or providers; this
//! module is the policy layer that joins them.

mod credential;
mod service;
mod store;

#[cfg(test)]
mod tests;

pub use credential::{CredentialService, CredentialStatus};
pub use service::{AccountService, AccountUpdate, AuthorizationContext, CreateAccount};
pub use store::AccountCredentialStore;
