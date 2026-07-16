# steam-local-kit

[![CI](https://github.com/LcpMarvel/steam-local-kit/actions/workflows/ci.yml/badge.svg)](https://github.com/LcpMarvel/steam-local-kit/actions/workflows/ci.yml)

English | [简体中文](README.zh-CN.md)

Product-neutral Rust primitives for discovering Steam installations, resolving
local users, inspecting per-app launch options, and safely editing
`localconfig.vdf`.

## Install

The crate is currently distributed from GitHub:

```toml
[dependencies]
steam-local-kit = { git = "https://github.com/LcpMarvel/steam-local-kit.git", tag = "v0.1.0" }
```

The Cargo package name is `steam-local-kit`; Rust code imports it as
`steam_local_kit`.

## Capabilities

- Discover platform-default Steam roots on Windows, macOS, and Linux.
- Parse additional library roots from `libraryfolders.vdf`.
- Discover `userdata` candidates and use `loginusers.vdf` as user-selection
  evidence.
- Represent missing, unique, and ambiguous user resolution explicitly.
- Read `LaunchOptions` for any caller-provided Steam App ID.
- Plan conservative `EnsureToken` and `RemoveToken` edits without writing.
- Apply edits with stale-source detection, verified backups, cooperative
  per-target locking, permission preservation, and same-directory atomic
  replacement.
- Verify and restore eligible backups while preserving a protection backup of
  the current file.
- Return typed errors and diagnostics without including VDF contents or launch
  option values in `Debug` output.

## Discover and inspect

```rust,no_run
use steam_local_kit::{
    additional_library_roots, platform_default_roots, read_launch_options, AppId, Resolution,
    SteamError, UserResolver,
};

fn main() -> Result<(), SteamError> {
    let roots = platform_default_roots();

    for steam_root in &roots {
        for library in additional_library_roots(steam_root).unwrap_or_default() {
            println!("{}", library.path.display());
        }
    }

    match UserResolver::resolve(&UserResolver::discover(&roots)) {
        Resolution::Unique(user) => {
            let options =
                read_launch_options(&user.local_config_path, AppId::new(123_456))?;
            println!("LaunchOptions present: {}", options.is_some());
        }
        Resolution::Missing { diagnostics } => {
            eprintln!("No local Steam user found: {diagnostics:?}");
        }
        Resolution::Ambiguous {
            candidates,
            diagnostics,
        } => {
            eprintln!(
                "Select one of {} candidates; diagnostics: {diagnostics:?}",
                candidates.len(),
            );
        }
    }

    Ok(())
}
```

`UserResolver` never guesses when the available evidence cannot identify one
user. The caller is responsible for presenting or applying a selection policy.

## Plan and apply an edit

Mutation is split into two explicit steps. Planning parses the current source
and produces an opaque edit plan bound to its byte length and SHA-256
fingerprint. Applying the plan rechecks the source, creates and verifies a
backup, and atomically replaces the target.

```rust,no_run
use std::{fs, path::Path};
use steam_local_kit::{
    AppId, LaunchOptionsEdit, LocalConfigDocument, SafeTransaction, WritePolicy,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new("/explicit/Steam/userdata/42/config/localconfig.vdf");
    let source = fs::read_to_string(path)?;
    let document = LocalConfigDocument::parse_at(path, &source)?;

    let plan = document.plan_launch_options_edit(
        AppId::new(123_456),
        LaunchOptionsEdit::EnsureToken("-example-token"),
    )?;

    let policy = WritePolicy::new(
        "my-product.backup-20260716-120000",
        "my-product.backup-",
    );
    let result = SafeTransaction::apply(path, &plan, &policy)?;

    assert_eq!(result.backup.is_some(), result.changed);

    Ok(())
}
```

A no-op plan does not create a backup. A changed transaction uses a hidden,
same-directory `.<target>.steam-local-kit.lock` sidecar so cooperating
processes fail closed instead of writing concurrently.

## Restore a backup

Backups must be regular, non-symlink files in the target directory and match
the caller-owned prefix:

```rust,no_run
use std::path::Path;
use steam_local_kit::{SafeTransaction, SteamError, WritePolicy};

fn main() -> Result<(), SteamError> {
    let target = Path::new("/explicit/Steam/userdata/42/config/localconfig.vdf");
    let backup = Path::new(
        "/explicit/Steam/userdata/42/config/my-product.backup-20260716-120000",
    );
    let policy = WritePolicy::new(
        "my-product.backup-before-restore",
        "my-product.backup-",
    );

    let verified =
        SafeTransaction::verify_backup(target, backup, "my-product.backup-")?;
    let result = SafeTransaction::restore(target, &verified, &policy)?;

    println!("Restored from {}", result.restored_from.display());

    Ok(())
}
```

Before restoration, the current target is saved as a protection backup.

## Safety and scope

- App IDs, tokens, backup naming, user interaction, and process-control policy
  belong to the caller.
- Token edits use conservative whitespace-delimited matching. This is not a
  general shell or Windows command-line parser.
- Planning is side-effect free, and callers cannot construct or modify an
  `EditPlan` replacement directly.
- Edits preserve bytes outside the targeted `LaunchOptions` field.
- A changed source, changed backup, invalid VDF, unsafe backup path, or failed
  verification causes the operation to stop.
- Cooperative locking coordinates users of this crate. It cannot prevent an
  unrelated process that ignores the lock from writing the same file.
- The API is synchronous and does not require an async runtime.

## Development

The CI workflow tests stable Rust on Linux, macOS, and Windows.

```bash
cargo test --all-targets
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```
