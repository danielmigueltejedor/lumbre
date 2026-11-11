//! Lumbre Código: the parts of a coding agent that do not need a model.
//!
//! * [`Workspace`] — a repository seen the way a developer sees it (ignored files stay ignored),
//!   with file, text and symbol search.
//! * [`patch`] — changes are unified diffs: planned against the working tree first, shown for
//!   review, and only then written.
//! * [`Permissions`] — nothing is written and nothing is executed unless the user allowed it for
//!   that repository. The grants live in the user's configuration, never inside the repository,
//!   so a cloned project cannot grant itself anything.
//!
//! The language model, when one is used, is reached through the Lumbre daemon; this crate starts
//! no server of its own.

pub mod patch;
mod permissions;
mod run;
mod symbols;
mod workspace;

pub use permissions::{Grant, PermissionError, Permissions};
pub use run::{run, RunError, RunOutput, READ_ONLY_GIT};
pub use symbols::{Symbol, SUPPORTED_EXTENSIONS};
pub use workspace::{Match, Overview, Workspace, WorkspaceError};
