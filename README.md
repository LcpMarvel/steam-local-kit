# steam-local-kit

Product-neutral Rust primitives for discovering, inspecting, and safely editing
local Steam configuration.

The `0.1` API includes:

- platform-default Steam roots and structured `libraryfolders.vdf` parsing;
- `userdata`/`loginusers.vdf` discovery with explicit missing, unique, and
  ambiguous user resolution, including canonical deduplication of symlinked
  root aliases;
- App-ID-parameterized `LaunchOptions` reads;
- side-effect-free, opaque, SHA-256-bound `EnsureToken`/`RemoveToken` edit
  plans;
- verified backups, stale-source detection, permission-preserving same-directory
  atomic replacement, per-target cooperative locking, and guarded restore;
- typed errors, diagnostics, and redacted `Debug` implementations that never
  copy VDF contents or Launch Options values.

It intentionally contains no Dota 2 IDs, GSI paths, backup brand prefix,
process-control policy, localized UI copy, Tauri, or async runtime dependency.

```rust
use steam_local_kit::{
    additional_library_roots, platform_default_roots, read_launch_options, AppId, Resolution,
    UserResolver,
};

let roots = platform_default_roots();
for steam_root in &roots {
    for library in additional_library_roots(steam_root).unwrap_or_default() {
        println!("{}", library.path.display());
    }
}

match UserResolver::resolve(&UserResolver::discover(&roots)) {
    Resolution::Unique(user) => {
        let options = read_launch_options(&user.local_config_path, AppId::new(570))?;
        println!("LaunchOptions present: {}", options.is_some());
    }
    Resolution::Missing { .. } | Resolution::Ambiguous { .. } => {
        // Ask the product/user to resolve the target; never guess.
    }
}
# Ok::<(), steam_local_kit::SteamError>(())
```

Mutation is split into planning and application: parse a
`LocalConfigDocument`, call `plan_launch_options_edit`, then explicitly pass the
result and a product-owned `WritePolicy` to `SafeTransaction::apply`. A no-op
plan does not create a backup. Changed transactions use a hidden, same-directory
`.<target>.steam-local-kit.lock` sidecar so all
cooperating consumers fail closed instead of racing through the final replace.
The empty sidecar is intentionally retained so every process locks the same
stable filesystem object.

```rust
use std::fs;
use steam_local_kit::{
    AppId, LaunchOptionsEdit, LocalConfigDocument, SafeTransaction, WritePolicy,
};

let path = std::path::Path::new("/explicit/Steam/userdata/42/config/localconfig.vdf");
let source = fs::read_to_string(path)?;
let document = LocalConfigDocument::parse_at(path, &source)?;
let plan = document.plan_launch_options_edit(
    AppId::new(570),
    LaunchOptionsEdit::EnsureToken("-example-token"),
)?;
let policy = WritePolicy::new("my-product.backup-20260716-120000", "my-product.backup-");
let result = SafeTransaction::apply(path, &plan, &policy)?;
assert_eq!(result.backup.is_some(), result.changed);
# Ok::<(), steam_local_kit::SteamError>(())
```

The public API and migration sequence are defined in
[`docs/steam-local-kit-public-api-rfc.md`](docs/steam-local-kit-public-api-rfc.md).
