# RFC：`steam-local-kit` 公共 API

- 状态：已实施（Phase A–D 完成，Phase E 发布准备待办）
- 日期：2026-07-16
- 目标版本：`0.1.0`

## 1. 决策

将 Steam 本地发现、用户解析、VDF 定点编辑和安全文件事务抽成无 Aegis、Dota、Tauri、Tokio 依赖的 Rust library crate：`steam-local-kit`。

首阶段将 crate 放在本仓库 `crates/steam-local-kit`，以便行为基线、调用方迁移和锁文件变更可以原子验证。公共 API 稳定且至少有第二个消费者后，可把源码与 API 原样拆到 `/Volumes/lcp/hovel/steam-local-kit/` 独立仓库（仅把 workspace 继承项改为独立 manifest 值）；消费者改用 Git tag，Rust 模块和类型无需变化。

## 2. 目标

- 跨 Windows、macOS、Linux 发现 Steam 安装和 Library roots。
- 游戏配置操作使用 `AppId` 参数，不在公共包硬编码产品名称或游戏 ID。
- 返回 Steam 用户候选、最近用户证据和歧义状态，不替产品做危险选择。
- 读取任意 App ID 的 `LaunchOptions`。
- 生成可审计、幂等、可验证的定点修改计划。
- 提供备份、竞争修改检测、原子替换和恢复原语。
- 保持源文件未修改区域的原始字节、顺序和格式。
- 让 CLI、Tauri、服务进程和测试使用同一套核心逻辑。

## 3. 非目标

- 不启动、停止或控制 Steam/游戏进程。
- 不内置 Dota 2、GSI 或 Coplaya 默认值。
- 不修改云端 Steam 数据或调用 Steam Web API。
- 不解析或暴露 `localconfig.vdf` 中与目标操作无关的认证、账户和社交数据。
- 不在 `0.1` 承诺通用 shell/Windows 命令行重写器；只提供保守、可验证的 token 操作。
- 不让 library 自行决定 UI 文案、重试策略或用户确认流程。

## 4. 依赖和分层约束

```text
Aegis/Tauri product adapter
        ↓
steam-local-kit public API
        ↓
lossless VDF + filesystem/platform adapters
```

依赖方向只能向下：

- `steam-local-kit` 不依赖任何 `aegis-*` crate。
- 核心 API 同步，避免强迫消费者选择异步 runtime。
- 平台注册表代码通过 `cfg(windows)` 隔离。
- 核心类型不依赖 `serde` 或 Tauri 序列化；若后续增加 `serde`，只能作为可选 feature。
- 用户可见本地化文案不进入 crate。

## 5. 模块结构

```text
src/
├── lib.rs
├── error.rs
├── discovery.rs       # Steam roots、libraryfolders
├── users.rs           # loginusers、userdata、用户解析证据
├── local_config.rs    # 只读语义访问和 lossless 文档索引
├── launch_options.rs  # 查询与 edit plan
└── transaction.rs     # 备份、指纹、原子应用、恢复
```

`process.rs` 不进入 `0.1`：Steam/Dota 进程名和阻止写入规则继续属于产品策略。以上模块已按第 12 节顺序实施。

## 6. 基础类型

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AppId(u32);

impl AppId {
    pub const fn new(value: u32) -> Self;
    pub const fn get(self) -> u32;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution<T> {
    Missing { diagnostics: Vec<Diagnostic> },
    Unique(T),
    Ambiguous { candidates: Vec<T>, diagnostics: Vec<Diagnostic> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub path: Option<PathBuf>,
}
```

约束：

- `Diagnostic` 只包含稳定 code 和必要路径，不包含 VDF 正文。
- `Resolution` 强迫调用方显式处理缺失与歧义。
- App ID 不接受产品名称或隐式默认值。

## 7. 发现 API

`0.1` 只搬迁当前产品实际使用的 root 与 Library registry 能力：

```rust
pub fn platform_default_roots() -> Vec<PathBuf>;
pub fn additional_library_roots(
    steam_root: &Path,
) -> Result<Vec<LibraryRoot>, SteamError>;

pub struct LibraryRoot {
    pub path: PathBuf,
    pub source: LibrarySource,
}
```

规则：

- `platform_default_roots` 返回确定性优先级并按路径去重。
- 已存在路径按 canonical 文件身份去重，符号链接或等价 root 不生成重复候选；对外仍保留首个输入路径。
- 调用方先加入 Steam root 本身，再追加 `additional_library_roots`。
- `libraryfolders.vdf` 使用 VDF 解析；单个损坏 root 返回 typed error，聚合多个 root 的调用方可对该 root fail-soft，而不抹掉其他候选。
- 附加库按 registry 数字 index 排序并去重；entry `0`（client root）不重复返回。
- `platform_default_roots()` 是读取环境/注册表的便利适配器；`additional_library_roots` 接受显式 root，便于确定性测试。

通过 `appmanifest_<appid>.acf` 解析任意已安装 App 的 `SteamLocator` 留给后续小版本；它不是当前 Aegis 逻辑搬迁的前置条件，也不属于本轮已发布 API。

## 8. 用户解析 API

```rust
pub struct SteamUserCandidate {
    pub account_id: u32,
    pub steam_id64: Option<u64>,
    pub local_config_path: PathBuf,
    pub most_recent: bool,
    pub evidence: Vec<UserEvidence>,
}

pub struct UserDiscovery {
    pub candidates: Vec<SteamUserCandidate>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct UserResolver;

impl UserResolver {
    pub fn discover<I, P>(steam_roots: I) -> UserDiscovery
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>;

    pub fn resolve(discovery: &UserDiscovery) -> Resolution<SteamUserCandidate>;
}
```

`resolve` 只在以下情况返回 `Unique`：

1. 只有一个有效候选；或
2. 多个候选中恰有一个具有可靠 `MostRecent` 证据。

否则必须返回 `Ambiguous`。

缺失、不可读或损坏的 `loginusers.vdf` 进入结构化 diagnostics，不隐藏仍然有效的 `localconfig.vdf` 候选；diagnostics 不携带文件正文、显示名或 Launch Options。

## 9. Launch Options 与 edit plan

```rust
pub enum LaunchOptionsEdit<'a> {
    EnsureToken(&'a str),
    RemoveToken(&'a str),
}

pub struct LocalConfigDocument<'a>;

impl<'a> LocalConfigDocument<'a> {
    pub fn parse(source: &'a str) -> Result<Self, SteamError>;
    pub fn parse_at(path: &Path, source: &'a str) -> Result<Self, SteamError>;
    pub fn launch_options(&self, app_id: AppId) -> Result<Option<&str>, SteamError>;
    pub fn plan_launch_options_edit(
        &self,
        app_id: AppId,
        edit: LaunchOptionsEdit<'_>,
    ) -> Result<EditPlan, SteamError>;
}

pub struct FileFingerprint {
    // byte length + SHA-256; raw source is never retained here
}

pub struct EditPlan {
    // fields private; only the planner can construct a transaction plan
}

impl EditPlan {
    pub const fn changed(&self) -> bool;
    pub const fn source_fingerprint(&self) -> &FileFingerprint;
    pub fn replacement(&self) -> &[u8];
    pub fn into_replacement(self) -> Vec<u8>;
    pub fn changes(&self) -> &[Change];
}
```

规则：

- 解析与规划不写文件。
- `EnsureToken` 幂等；完整 token 命中才算存在。
- `RemoveToken` 只删除完整空白 token，不重排其他参数；目标字段内部空白会规范化为单个空格。
- `Change` 只描述字段路径、操作类型和字节范围，不复制敏感原值。
- 无法证明修改范围唯一时返回错误，不进行猜测性重写。
- `EditPlan` 用源字节长度与 SHA-256 绑定规划时的输入，事务层不接受 stale plan。
- `EditPlan` 字段保持私有，调用方只能读取或消费规划结果，不能伪造任意 replacement。
- `LocalConfigDocument` 与 `EditPlan` 的 `Debug` 只输出路径、长度、指纹和修改元数据，不输出 VDF 正文。

## 10. 安全事务 API

```rust
pub struct WritePolicy {
    pub backup_name_stem: String,
    pub allowed_backup_prefix: String,
    pub require_unchanged_source: bool,
    pub preserve_permissions: bool,
}

pub struct SafeTransaction;

impl SafeTransaction {
    pub fn apply(
        path: &Path,
        plan: &EditPlan,
        policy: &WritePolicy,
    ) -> Result<ApplyResult, SteamError>;

    pub fn verify_backup(
        target_path: &Path,
        backup_path: &Path,
        allowed_backup_prefix: &str,
    ) -> Result<VerifiedBackup, SteamError>;

    pub fn restore(
        path: &Path,
        backup: &VerifiedBackup,
        policy: &WritePolicy,
    ) -> Result<RestoreResult, SteamError>;
}
```

`apply` 固定执行：

1. 比较当前源指纹与 plan 指纹。
2. 验证 replacement 是 UTF-8 VDF，且 plan 的 `changed`/changes/指纹自洽。
3. 获取目标专属的同目录 sidecar 协作锁；Unix 同时持有目标文件 advisory lock。
4. 在锁内重新确认源未变化。
5. 创建并字节验证备份。
6. 再次确认源未变化。
7. 在同目录以安全权限创建唯一临时文件。
8. 写入、刷新并原子替换。
9. 重新读取并逐字节验证 replacement。

`verify_backup` 只接受目标同目录、匹配产品 prefix、非 symlink 的普通 VDF 文件；`restore` 会再次验证该备份的指纹，并在覆盖前为当前配置创建 protection backup。

同一目标的所有 `steam-local-kit` 写事务通过隐藏 sidecar 锁 fail-closed；空 sidecar 会保留，以确保不同进程始终锁定同一个稳定文件对象。Unix 目标文件锁也会约束遵守 advisory lock 的其他写入方。无法遵守锁的外部进程仍由产品层进程门禁和锁内最终重读共同防护，公共包不宣称提供跨任意外部进程的文件系统 CAS。

产品层传入完整的 `backup_name_stem`，因此 Aegis 继续使用 `localconfig.vdf.back-YYYYMMDD-HHMMSS[-before-restore]`；公共包不内置品牌前缀或时区/显示格式。

产品可在事务成功后继续执行 App/token 语义后置验证。进程是否运行由产品层在调用前判断；library 不探测、启动或终止进程。

## 11. 错误模型

```rust
pub enum SteamError {
    Io {
        operation: IoOperation,
        path: PathBuf,
        source: std::io::Error,
        recovery_path: Option<PathBuf>,
    },
    InvalidVdf { path: Option<PathBuf>, reason: String },
    MissingNode { path: Option<PathBuf>, node: String },
    AmbiguousNode { path: Option<PathBuf>, node: String },
    InvalidEdit { reason: &'static str },
    InvalidPolicy { reason: &'static str },
    SourceChanged { path: PathBuf, recovery_path: Option<PathBuf> },
    BackupChanged { path: PathBuf, recovery_path: Option<PathBuf> },
    BackupVerificationFailed { path: PathBuf },
    UnsafeBackup { path: PathBuf },
    WriteVerificationFailed { path: PathBuf, recovery_path: PathBuf },
}
```

要求：

- variant 和 code 稳定，`Display` 仅供开发诊断。
- 产品自行把错误映射为中文/英文文案。
- 错误不得携带 VDF 全文、Launch Options 原值、认证信息或用户显示名。
- 写入边界之后的失败通过 `recovery_path()` 暴露已验证备份位置，产品可安全提示恢复入口。
- `IoOperation::Lock` 表示目标已有协作事务或锁文件无法安全打开。

## 12. 迁移顺序

### Phase A：只读发现（已完成）

- 新建 crate 和 `discovery`。
- 搬迁 Steam 平台 root 与 `libraryfolders.vdf` 解析。
- `aegis-live` 改用公共 API，保持 `detect_gsi_dir` 对外契约。

### Phase B：用户和只读 Launch Options（已完成）

- 搬迁 `loginusers.vdf`、userdata 和 `read_option`。
- Desktop 保持现有 `Inspection`/`SetupStatus`，只替换底层实现。

### Phase C：edit plan（已完成）

- 搬迁 lossless tokenizer 和 patch 逻辑。
- 新旧实现对同一 fixtures 生成逐字节相同结果；允许的修复由基线文档第 8 节管理。

### Phase D：安全事务（已完成）

- 搬迁备份、源指纹、原子写入和恢复。
- Aegis 保留进程门禁、用户确认、文案和 backup namespace。

### Phase E：独立发布（待办）

- Windows/macOS/Linux CI。
- 第二个项目使用 Git dependency 接入。
- 标记 `0.1.0`，再评估拆仓和 crates.io 发布。

## 13. 验收标准

- 行为基线测试保持通过。
- 公共 crate 无 `aegis-*`、Tauri、Tokio 依赖。
- `cargo test -p steam-local-kit` 独立通过。
- `aegis-live` 和 `aegis-desktop` 原有公共契约不变。
- 多用户仍 fail-closed。
- 对 fixture 的非目标字节保持一致。
- 写入错误和诊断不泄露 VDF 内容。
- 发布前，Windows、macOS、Linux 至少完成 compile/test CI；Windows 原子替换需要目标平台运行测试。

Phase A–D 及 review hardening 的本地验收结果记录在[行为基线第 10 节](./steam-local-kit-behavior-baseline.md#10-phase-ad-抽取及-review-hardening-后证据)：公共包 41 项、全 workspace 438 项测试通过，Windows/x86_64 Linux/aarch64 Linux target check 通过；Windows 运行测试保留为 Phase E 门禁。

## 14. 暂不阻塞实施的开放项

- 最终 crate/repository 名称是否保留 `steam-local-kit`。
- `serde` 是否进入 `0.1` feature。
- 发布前的 MSRV 和 license。
- 是否在 `0.2` 提供通用的 Steam 进程探测实现。

这些项目不影响当前仓库内 package 的复用，也不阻塞其他项目先通过 path/Git dependency 接入；它们在独立发布前确定。
