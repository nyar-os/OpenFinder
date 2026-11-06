//! OpenFinder domain core.
//!
//! This crate owns NTFS index semantics: direct `$MFT` record parsing, USN
//! journal incremental updates, index persistence, query matching, and the
//! explicit non-index portable walk. Platform-specific volume access is
//! confined to this crate; the N-API boundary stays platform-neutral.
//!
//! Capability failures use exactly three stable codes:
//! `unsupported_platform`, `not_implemented`, and `dependency_blocked`.

mod audit;
mod config;
mod error;
mod index;
mod install;
mod mft;
mod ntfs;
pub mod platform;
mod query;
mod usn;
mod volume;
mod walk;

pub use audit::{AuditFields, capability_ids};
pub use config::{Config, config_dir, config_path, default_index_dir, load_config};
pub use error::{CapabilityState, Error, ErrorPayload, Result, capability_code};
pub use index::{
    BuildOptions, BuildReport, Index, IndexEntry, IndexMeta, UpdateReport, VolumeReport, build_index, inspect_volume, load_index, update_index,
};
pub use install::{
    APP_ID, APP_TITLE, ActionKind, InstallOptions, InstallPlan, PlannedAction, UninstallOptions, UninstallPlan, apply_install, apply_uninstall,
    plan_install, plan_install_with, plan_uninstall, plan_uninstall_with,
};
pub use platform::{Capabilities, Capability, Platform, capabilities, platform_arch, platform_name};
pub use query::{EntryKind, QuerySpec, SearchHit, SearchPage, SortKey, search};
pub use walk::{WalkOptions, walk};
