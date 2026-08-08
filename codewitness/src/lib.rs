//! `codewitness`: deterministic, evidence-grounded staleness detection for
//! code-anchored claims.
//!
//! Zero LLM. Zero wall-clock time. Every [`Verdict`] is derived from
//! content hashes and git commit-graph ancestry alone — see
//! [`causal::compare`] for why timestamps are never consulted.
//!
//! # Quick start
//! ```no_run
//! use codewitness::{Anchor, Auditor};
//!
//! let auditor = Auditor::discover(".")?;
//! let anchor = Anchor::new("src/lib.rs").with_symbol("Auditor::stamp");
//! let witness = auditor.stamp(&anchor)?;
//!
//! // ... time passes, the repository changes ...
//!
//! let verdict = auditor.try_audit(&witness)?;
//! println!("{verdict:?}");
//! # Ok::<(), codewitness::Error>(())
//! ```

mod anchor;
mod auditor;
pub mod causal;
mod diff_id;
mod error;
mod stamp;
mod verdict;
mod witness;

pub use anchor::Anchor;
pub use auditor::Auditor;
pub use causal::CausalOrder;
pub use diff_id::{normalized_diff_id, DiffId};
pub use error::{Error, Result};
pub use stamp::{stamp_normalized, Stamp, StampKind};
pub use verdict::{SupersededReceipt, SupersessionBasis, Verdict};
pub use witness::{Tier, Witness};

/// Re-exported so downstream crates can construct [`Witness::at`] /
/// [`SupersededReceipt::receipt`] values without a direct `gix`
/// dependency of their own.
pub use gix::ObjectId;
