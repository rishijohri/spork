//! The unit of permission: [`Grant`].
//!
//! A [`Grant`] pairs one [`Capability`] with the [`Scope`] the user has
//! authorized for it. The broker is constructed from a list of grants
//! ([`CapabilityBroker::new`](crate::CapabilityBroker::new)); together they are
//! the *entire* set of side effects an executor may perform. Anything not
//! covered by a grant is denied (deny-by-default, DESIGN.md §15.2).
//!
//! Grants are persisted (they are what the user reviewed and approved at
//! install), so the embedded [`Scope`] carries its own `schema_version`. The
//! grant set per node-type template is the "capability bundle" the design ships
//! to avoid permission fatigue.
//!
//! Design references: DESIGN.md §15.2 (a manifest declares a fixed, typed
//! capability vocabulary; the user reviews and grants at install).

use serde::{Deserialize, Serialize};

use crate::capability::Capability;
use crate::scope::Scope;

/// One authorized capability and the scope it is authorized within.
///
/// Multiple grants for the *same* capability are permitted and are treated as a
/// union: a request is allowed if **any** grant for that capability covers it.
/// This lets a bundle express "reads under `src/**` and reads under `tests/**`"
/// as two grants without a combined glob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// The capability this grant authorizes.
    pub capability: Capability,
    /// The scope within which `capability` is authorized.
    pub scope: Scope,
}

impl Grant {
    /// Construct a grant pairing `capability` with `scope`.
    #[must_use]
    pub fn new(capability: Capability, scope: Scope) -> Self {
        Grant { capability, scope }
    }
}
