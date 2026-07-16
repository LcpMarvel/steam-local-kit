# Steam 本地集成行为基线

- 状态：已锁定
- 日期：2026-07-16
- 适用范围：抽取 `steam-local-kit` 前的 Aegis/Coplaya Steam 本地发现与配置行为

## 1. 目的

本文记录抽包前必须保持的可观察行为。它不是理想化设计；已知缺陷单独列在第 8 节，迁移时只有在 RFC 明确授权且新增测试后才能改变。

当前实现分布在：

- `apps/aegis-desktop/src-tauri/src/steam_setup.rs`：Steam 用户解析、Dota 2 启动参数、备份、原子写入、恢复和进程门禁。
- `apps/aegis-live/src/gsi.rs`：Steam 根目录、库目录和 Dota 2 GSI 目录发现。
- `apps/aegis-desktop/src-tauri/src/lib.rs`：将 Steam、GSI 和产品 UI 状态编排为 `SetupStatus`。

## 2. 术语

- **Steam root**：包含 `steamapps`、`userdata` 和 `config` 的 Steam 客户端目录。
- **Library root**：包含 `steamapps/common` 的 Steam 游戏库目录。
- **Local config**：`userdata/<account-id>/config/localconfig.vdf`。
- **目标 App**：当前固定为 Dota 2，Steam App ID `570`。
- **目标 token**：当前固定为 `-gamestateintegration`。
- **无损定点修改**：除目标 `LaunchOptions` 字符串或缺失字段的插入位置外，其他源字节保持不变。

## 3. 发现行为

| ID | 当前行为 | 兼容要求 |
| --- | --- | --- |
| DISC-01 | GSI 显式路径优先于 `DOTA2_GSI_DIR` 和平台默认路径。 | 保持优先级。 |
| DISC-02 | GSI 候选按规范化路径去重，首个目录已存在或父目录存在的候选被选中。 | 保持顺序、去重和“可创建”语义；已存在的符号链接别名按 canonical 文件身份合并。 |
| DISC-03 | 平台默认 Steam root 来自 Windows 环境变量/注册表、macOS Steam 默认目录或 Linux 常见目录。 | 新包可增加诊断和候选，但不得降低现有平台覆盖。 |
| DISC-04 | `libraryfolders.vdf` 中非空 `path` 按文件出现顺序返回，Windows 双反斜杠被还原。 | 保持有序、去除无关字段和 Windows 路径解码。 |
| DISC-05 | 找不到或无法读取 `libraryfolders.vdf` 时返回空附加库列表，不中断默认 root 检测。 | 保持 fail-soft。 |

## 4. Steam 用户行为

| ID | 当前行为 | 兼容要求 |
| --- | --- | --- |
| USER-01 | 从每个 Steam root 的 `userdata/*/config/localconfig.vdf` 生成用户候选。 | 保持只读取必要路径。 |
| USER-02 | 多个候选时读取 `config/loginusers.vdf` 的 `MostRecent=1`，将 SteamID64 转为 account ID 后筛选。 | 保持选择证据；新包必须把证据暴露给调用方。 |
| USER-03 | 唯一候选才允许自动配置；零候选或仍有多个候选时 fail-closed。 | 不得任意选择用户；指向同一 Steam root/localconfig 的路径别名不得制造伪歧义。 |
| USER-04 | 用户解析失败不会读取、记录或上传 `localconfig.vdf` 的无关字段。 | 保持隐私边界。 |

## 5. Launch Options 行为

| ID | 当前行为 | 兼容要求 |
| --- | --- | --- |
| OPT-01 | 读取 VDF 路径 `Software/Valve/Steam/apps/570/LaunchOptions`。 | 新包参数化 App ID，Aegis 仍传入 `570`。 |
| OPT-02 | 目标 token 按空白分隔后的完整 token 匹配，子串不算命中。 | `foo-gamestateintegration` 和 `-gamestateintegration-extra` 必须不命中。 |
| OPT-03 | 已存在目标 token 时不修改文件，也不创建无意义备份。 | 保持幂等。 |
| OPT-04 | 字段存在且非空时在原值尾部追加一个空格和目标 token。 | 保持现有参数、Unicode、引号和反斜杠。 |
| OPT-05 | 字段缺失时只在 App `570` 对象内插入 `LaunchOptions`。 | 其他 App（例如 `730`）及其他字段不得改变。 |
| OPT-06 | VDF 无法解析或目标 App 路径不存在时拒绝自动修改。 | 在引入显式“创建 App 节点”API 前保持 fail-closed。 |

## 6. 写入、备份与恢复行为

| ID | 当前行为 | 兼容要求 |
| --- | --- | --- |
| WRITE-01 | Dota 2 或 Steam 仍运行时拒绝写入和恢复。 | 规则保留在 Aegis 策略层；公共包只提供可注入门禁。 |
| WRITE-02 | 实际修改前在原目录创建完整字节备份，默认前缀为 `localconfig.vdf.back-`。 | 公共包参数化 namespace/prefix，Aegis 保持现有命名。 |
| WRITE-03 | 同秒重名时追加 `-1`、`-2`；备份与源文件字节不一致时删除失败备份并停止。 | 保持。 |
| WRITE-04 | 备份后、写入前再次读取源文件；如果字节变化则停止，避免 TOCTOU 覆盖。 | 新包使用源指纹，并在最终检查与替换期间持有同目标 sidecar 协作锁；Unix 同时持有 advisory 目标锁。 |
| WRITE-05 | 临时文件与目标文件同目录，写入后 `sync_all`，继承权限并原子替换。 | 保持；权限必须在写入敏感正文之前应用，Windows 继续使用替换语义而不是先删后写。 |
| WRITE-06 | 写入后重新读取并验证目标 token。 | 保持后置验证。 |
| RESTORE-01 | 只允许恢复目标目录内、符合 Aegis 前缀且当前可枚举的备份。 | 保持路径和来源限制。 |
| RESTORE-02 | 恢复前为当前文件再创建 `-before-restore` 保护备份。 | 保持。 |
| RESTORE-03 | 备份必须是 UTF-8 且可解析为 VDF；恢复后字节必须与备份一致。 | 保持。 |

## 7. 产品层行为

- 公共逻辑返回结构化状态；中文/英文用户文案由 Aegis 映射。
- `SetupStatus` 的现有字段和 Tauri command 名称在首轮迁移中不改变。
- Dota App ID、GSI 相对目录、目标 token、进程名和是否允许写入均属于 Aegis 策略，不进入公共包默认值。
- 公共包错误、日志和 `Debug` 输出不得包含完整 VDF 内容或无关字段值。
- 公共 edit plan 必须由 planner 构造；消费者不能替换其正文或伪造修改元数据。

## 8. 已知限制，不作为兼容目标

以下问题允许在抽取过程中修复，但必须有新的公共包测试和迁移说明：

1. Desktop 与 Live 各自维护一套 Steam root 发现规则，可能漂移。
2. `libraryfolders.vdf` 当前使用逐行字符串提取，不是完整 VDF 语义解析。
3. Launch Options 当前以 `split_whitespace` 判断 token，不能完整表达所有平台的命令行引用规则。
4. App ID、token、备份前缀和用户文案硬编码在实现中。
5. `tasklist`/`pgrep` 进程检查只能作为策略门禁，不能消除检查后进程重新启动的竞态。
6. 恢复会覆盖整个 `localconfig.vdf`；公共包应优先支持可逆的定点 edit plan。
7. 当前测试需要真实进程和环境变量的部分不可注入，公共包必须提供确定性输入接口。

## 9. 自动化证据

抽取前基线命令：

```bash
cargo test -p aegis-desktop --lib steam_setup::tests -- --nocapture
cargo test -p aegis-live --lib gsi::tests -- --nocapture
```

2026-07-16 结果：

- Desktop Steam：9 passed。
- Live GSI/Steam discovery：6 passed。
- 覆盖精确 token、幂等判定、跨 App 定点修改、Unicode/引号/反斜杠、备份原子替换、附加库顺序、Windows 路径和显式 GSI 路径优先级。

## 10. Phase A–E 抽取、发布及 review hardening 后证据

完成公共包、调用方迁移和独立 GitHub 仓库后：

- 独立仓库 `/Volumes/lcp/hovel/steam-local-kit` 的 `cargo test --all-targets`：41 passed，覆盖发现、canonical 路径别名去重、用户证据、只读访问、lossless edit plan、VDF 控制字符转义、脱敏 `Debug`、opaque plan、协作事务锁、stale plan、备份冲突、恢复保护备份、非法路径、敏感内容不回显和写入前权限收紧。
- Desktop Steam 行为基线：9 passed。
- Live GSI/Steam discovery 行为基线：6 passed。
- `cargo test --workspace`：438 passed、0 failed。
- 独立仓库 `cargo clippy --all-targets -- -D warnings`：通过。
- `x86_64-pc-windows-gnu`、`x86_64-unknown-linux-gnu` 与 `aarch64-unknown-linux-gnu`：独立包 target check 通过；macOS 本机执行全部公共包测试。
- GitHub 仓库：`https://github.com/LcpMarvel/steam-local-kit`，初始 tag `v0.1.0`。
- 真实只读 smoke：`aegis setup detect` 仍选择 `/Volumes/lcp/SteamLibrary` 下现有 Dota 2 `gamestate_integration` 目录。

Windows `ReplaceFileW` 路径已完成目标编译；独立仓库 CI 在 Windows runner 执行运行测试。
