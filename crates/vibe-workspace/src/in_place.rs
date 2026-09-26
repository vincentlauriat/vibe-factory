//! Working directly in the project directory, without isolation.
//!
//! [`InPlaceWorkspace`] is defined in `vibe-core` and re-exported here so
//! that every workspace provider can be reached from this crate. It is the
//! right choice for projects that are not git repositories, for throw-away
//! experiments, or when a human wants to watch files change live. Its
//! `merge` is a no-op returning [`vibe_core::workspace::MergeOutcome::NoChanges`]
//! and its `discard` leaves the directory untouched.
//!
//! Select it by name with [`crate::provider_by_name`]`("in_place", None)`.

pub use vibe_core::workspace::InPlaceWorkspace;
