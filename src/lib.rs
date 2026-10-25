//! Product-neutral primitives for inspecting Steam installations on the local
//! machine.
//!
//! The crate deliberately does not contain game IDs, UI copy, or mutation
//! defaults, and it never decides on its own to stop Steam: it offers the
//! probe and the polite shutdown request, and consumers decide when (and
//! whether the user agreed). Those decisions stay in their product adapter.

#![deny(unsafe_code)]

mod app_manifest;
mod discovery;
mod error;
mod launch_options;
mod local_config;
mod process;
mod transaction;
mod users;

pub use app_manifest::{find_app_install, AppInstall};
pub use discovery::{additional_library_roots, platform_default_roots, LibraryRoot, LibrarySource};
pub use error::{IoOperation, SteamError};
pub use launch_options::{Change, ChangeKind, EditPlan, FileFingerprint, LaunchOptionsEdit};
pub use local_config::{read_launch_options, LocalConfigDocument};
pub use process::{
    process_status, request_steam_shutdown, start_steam, steam_status, wait_for_steam_exit,
    ProcessStatus, STEAM_PROCESS_NAMES,
};
pub use transaction::{ApplyResult, RestoreResult, SafeTransaction, VerifiedBackup, WritePolicy};
pub use users::{
    Diagnostic, DiagnosticCode, Resolution, SteamUserCandidate, UserDiscovery, UserEvidence,
    UserResolver,
};

/// A Steam application identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AppId(u32);

impl AppId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for AppId {
    fn from(value: u32) -> Self {
        Self::new(value)
    }
}

impl From<AppId> for u32 {
    fn from(value: AppId) -> Self {
        value.get()
    }
}

#[cfg(test)]
mod tests {
    use super::AppId;

    #[test]
    fn app_id_round_trips_without_product_defaults() {
        let app_id = AppId::new(570);
        assert_eq!(app_id.get(), 570);
        assert_eq!(u32::from(app_id), 570);
    }
}
