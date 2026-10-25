# steam-local-kit

[![CI](https://github.com/LcpMarvel/steam-local-kit/actions/workflows/ci.yml/badge.svg)](https://github.com/LcpMarvel/steam-local-kit/actions/workflows/ci.yml)

[English](README.md) | 简体中文

用于发现 Steam 安装、解析本地用户、读取各 App 启动项，以及安全编辑
`localconfig.vdf` 的跨平台 Rust 基础库。

## 安装

该 crate 目前通过 GitHub 分发：

```toml
[dependencies]
steam-local-kit = { git = "https://github.com/LcpMarvel/steam-local-kit.git", tag = "v0.2.0" }
```

Cargo 包名是 `steam-local-kit`，在 Rust 代码中通过 `steam_local_kit`
引入。

## 功能

- 发现 Windows、macOS 和 Linux 上的 Steam 默认根目录。
- 从 `libraryfolders.vdf` 解析额外的 Steam 库目录。
- 发现 `userdata` 用户候选，并使用 `loginusers.vdf` 作为用户选择证据。
- 显式表达用户缺失、唯一和存在歧义三种解析结果。
- 通过库目录里的 `appmanifest_<id>.acf` 找到应用的安装目录；`installdir`
  不是单个普通目录名时拒绝。
- 根据调用方传入的任意 Steam App ID 读取 `LaunchOptions`；从未运行过该应用的
  用户视为没有启动项。
- 在不写入文件的前提下（用户还没有该应用节点时，`EnsureToken` 会创建它），规划保守的 `EnsureToken` 和 `RemoveToken`
  修改。
- 应用修改时提供源文件变化检测、备份校验、按目标协作锁、权限保留和
  同目录原子替换。
- 判断 Steam（或调用方指定的进程）是否在运行，像 Steam 自己的菜单那样请求
  它退出、等待它完全退出，并重新启动它。不会强杀任何进程；无法判断时绝不
  视为已退出。
- 校验并恢复符合条件的备份，同时为恢复前的当前文件保留保护备份。
- 返回类型化错误和诊断信息，`Debug` 输出不会包含 VDF 正文或启动项
  原值。

## 发现并读取配置

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
            println!("是否存在 LaunchOptions：{}", options.is_some());
        }
        Resolution::Missing { diagnostics } => {
            eprintln!("未找到本地 Steam 用户：{diagnostics:?}");
        }
        Resolution::Ambiguous {
            candidates,
            diagnostics,
        } => {
            eprintln!(
                "需要从 {} 个候选用户中选择；诊断信息：{diagnostics:?}",
                candidates.len(),
            );
        }
    }

    Ok(())
}
```

当现有证据无法唯一识别用户时，`UserResolver` 不会自行猜测。调用方需要
展示候选用户，或实现自己的选择策略。

## 规划并应用修改

写入被拆分为两个明确步骤。规划阶段解析当前内容，生成一个不透明的修改
计划，并通过字节长度和 SHA-256 指纹绑定源文件。应用阶段会重新检查源
文件、创建并校验备份，然后原子替换目标文件。

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

无需修改的计划不会创建备份。发生修改时，事务会使用同目录下隐藏的
`.<target>.steam-local-kit.lock` sidecar，使遵守该锁的多个进程在并发
写入时安全失败。

## 恢复备份

备份必须位于目标文件所在目录，是非符号链接的普通文件，并且文件名符合
调用方指定的前缀：

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

    println!("已从 {} 恢复", result.restored_from.display());

    Ok(())
}
```

执行恢复前，当前目标文件会先被保存为保护备份。

## 安全边界

- App ID、token、备份命名、用户交互和进程控制策略由调用方决定。
- Token 修改采用保守的空白分隔匹配；它不是通用的 Shell 或 Windows
  命令行解析器。
- 规划过程没有副作用，调用方无法直接构造 `EditPlan`，也不能替换其中的
  写入内容。
- 修改会保留目标 `LaunchOptions` 字段之外的原始字节。
- 如果源文件或备份发生变化、VDF 无效、备份路径不安全或写入校验失败，
  操作会立即停止。
- 协作锁用于协调本 crate 的使用者，但无法阻止完全忽略该锁的其他进程
  写入同一文件。
- API 是同步的，不要求调用方使用异步运行时。

## 开发

CI 使用 stable Rust 在 Linux、macOS 和 Windows 上运行测试。

```bash
cargo test --all-targets
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```
