# Changelog

## 0.2.0

- `find_app_install` reads an app's `appmanifest_<id>.acf` in a library and
  returns where Steam installed it. A manifest for another app is
  `SteamError::AppMismatch`; an `installdir` that is not one plain directory
  name is `SteamError::UnsafeInstallDir`.
- `LocalConfigDocument::launch_options` and `read_launch_options` return
  `Ok(None)` when the user has no node for the app (or no `apps` node), instead
  of `SteamError::MissingNode`. A user who never ran the app is common.
- `EnsureToken` creates the missing app node (and `apps` above it) inside the
  deepest existing object; the plan's change is `ChangeKind::InsertAppNode`.
  `RemoveToken` on a missing node changes nothing.
- Inserted `LaunchOptions` lines are indented like their siblings.
- New process primitives: `steam_status`, `process_status`,
  `request_steam_shutdown`, `wait_for_steam_exit` and `start_steam`, with
  `SteamError::ProcessProbeFailed`, `SteamError::SteamStillRunning` and
  `IoOperation::Launch`. Shutdown is a request (`steam.exe -shutdown`,
  `steam://exit`, `steam -shutdown`); nothing kills a process.

## 0.1.0

- First release.
