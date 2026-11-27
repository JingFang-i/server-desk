# R10 验收：提醒通道、设置持久化与服务器列表拖拽

> 本轮处理 5 个线上问题 + 2 个新特性。改动横跨前端（Vue）与桌面端（Rust），
> 涉及 SQLite 增量迁移；安全管线本身（guard 判定逻辑）未放松任何一条规则。

## 一、问题复盘与修复

### 1. AI 实时活动窗口「不显示服务器名」

**症状**：活动窗口里一部分条目显示服务器名，一部分空白或显示一串 UUID。

**根因**：`ipc.rs` 把 AI 传入的 `server` 参数原样回显。MCP 侧 `server` 既可能是服务器
**id（UUID）**也可能是**名称**：

- 传名称 → 窗口显示名称（所以「有一些显示了」）
- 传 id → 窗口显示 UUID（看起来像没显示）
- `exec_batch` 走 `servers`/`group` 参数，`server` 为空 → 显示「全局」

**修复**：新增 `ipc::activity_server_label()`，按 id / 名称统一解析成资产库里的**服务器名**；
`exec_batch` 汇总为「a、b、c 等 N 台」或「分组「prod」」；解析不到时脱敏回显。
这里只读资产表、仅用于展示，不参与任何安全判定。

### 2. 本地审批提示音从来没响过

**症状**：AI 请求审批时应用内有提醒卡，但没有声音。

**根因**（两层）：

- `alert.ts` 用 `window.addEventListener(..., { once: true })` **只尝试解锁一次** WebView
  音频上下文。那次若失败（页面对手手势判定不通过、输出设备刚切换、WebView 挂起），
  之后所有审批永远静音，且界面上没有任何补救入口（提示音未启用的横幅此前已被移除）。
- 上下文仍 `suspended` 时，`playApprovalChime` 直接把振荡器排进时间轴再 `resume()`，
  时间轴不前进 → 无声。整条链路完全依赖 WebView，Web Audio 一旦不可用就没有兜底。

**修复**：

- `armAlertSound()`：持续监听指针/键盘/焦点事件，**重试到音频上下文真正 running 为止**。
- `playApprovalChime()`：先 `resume`，确认 `running` 后再排音。
- 前端把「能否出声」上报后端（`alert_audio_ready`）；`notify.rs` 在
  **窗口在前台** 或 **系统通知投递失败** 且 Web Audio 出不了声时，直接用 `NSSound`
  播系统音（macOS 主线程；Windows/Linux 有对应兜底命令）。两条通道不会双响。
- 设置页新增「试听系统音」，可在不依赖 WebView 的情况下验证 OS 声音通道。

### 3. 每次点进设置都自动检查更新

**根因**：`UpdatePrompt` 在 `App.vue`（全局，负责启动检查）和 `SettingsView.vue`
各挂了一个实例；设置页实例每次挂载都会 `setTimeout(3000, checkUpdate)`。

**修复**：`UpdatePrompt` 新增 `autoCheck` 属性（默认 `true`）；设置页实例传
`:auto-check="false"`。启动静默检查仍只有一次。

### 4. 只读模式「开了又变回没开」

**根因**（两个独立缺陷叠加）：

1. **保存入口缺失**：只读模式、速率限制、严格分组在「AI 高级策略」卡片里，
   但该卡片没有保存按钮——保存按钮在下面的「命令安全规则」卡片里。用户拨完开关就离开，
   实际什么都没写库，重进设置自然回到关闭。
2. **反序列化字段名不匹配**：`GuardRules` 是 `snake_case`，
   前端提交的是 camelCase（`whitelistMode` / `ratePerMin` / `strictGroups`），
   叠加 `#[serde(default)]` 后这些键被当成未知字段**静默丢弃**——
   白名单模式、速率限制、严格分组从来存不进去（`readonly` 是单词恰好命中，
   所以看起来「有的能存、有的不能」）。

**修复**：

- `GuardRules` 加 `#[serde(rename_all = "camelCase")]` + 旧字段 `alias`，
  旧库里已存的 snake_case JSON 仍能读（不会把用户规则重置成默认值）。
- 「AI 高级策略」卡片加独立保存按钮；保存后用后端返回值回填表单，
  卡片标题显示「有未保存的改动 / 只读模式已生效」。
- **只读模式本身是否生效已复核**：`guard.evaluate()` 在泄露防护之后、黑名单之前拦截写入类
  命令（按子命令逐段判定、剥掉 `/dev/null` 重定向、拒命令替换）；文件写入/追加/上传/回滚
  在 `ops.rs` 里另有 `is_readonly()` 硬拒。判定链没有放开任何一条规则。

### 5. 系统通知不工作

**现场证据**（本机）：

```bash
$ defaults read com.apple.ncprefs | grep -c serverdesk
0          # 系统通知设置里从来没有 ServerDesk 这一条
```

**根因**：应用从未申请过系统通知授权。`tauri-plugin-notification` 在 macOS 上走
`notify-rust` → `NSUserNotificationCenter`（macOS 10.14 起弃用）：这套 API 不会把应用
注册进「系统设置 → 通知」，未授权进程的投递被系统**静默丢弃**；
而应用里的权限查询走的是另一套 `UNUserNotificationCenter`——
「查得到状态」和「发得出通知」是两条不同的链路，查出来能授权也不代表发得出去。

**修复**：

- 启动时（**无条件**）用 `UNUserNotificationCenter` 申请一次授权。第一版写成「先查状态、
  只有 `prompt` 才申请」，而启动瞬间的查询容易超时/调度失败返回 `unknown`，申请被整个跳过
  ——这正是「应用从来没进过通知列表」的原因。`requestAuthorization` 在已决定时会立即回调、
  不再弹框，所以每次启动调用都是安全的。
- macOS 投递改为 `UNNotificationRequest`（带默认提示音），不再依赖已弃用的
  `NSUserNotificationCenter`；非 macOS / 开发模式回退到插件。
- 投递结果写审计：`notify_permission`（启动授权结果）、`notify_test`（测试投递详情，
  含 `auth / focused / channel / delay / accepted / error`）。通知失败是静默的，
  不落审计就只能靠猜。
- 「发送测试通知」通过 `addNotificationRequest` 的**完成回调**回读 `NSError`，
  系统拒绝时把原文提示出来。
- 正式审批通知也等待同一投递回调。后台投递失败且 Web Audio 不可用时，播放原生提示音。
- **前台不弹横幅是 macOS 的规则**：没有实现 `UNUserNotificationCenterDelegate` 的应用，
  前台到达的通知会被系统直接丢弃（不是故障、也查不到错误）。因此测试通知在窗口前台时
  **延迟 5 秒**（`UNTimeIntervalNotificationTrigger`），让用户有时间切到别的应用；
  真实审批在前台则由应用内提醒卡 + 提示音承担。

**验证记录**（2026-11-26，本机 macOS，打包版）：

```
notify_test | ok | auth=granted focused=true channel=native delay=Some(5.0) accepted=true error=none
```

即系统接受了请求且无报错；按提示切到 Finder 后横幅正常出现，点「试听系统音」能听到 Glass 音。

## 二、新特性

### 6. 服务器列表拖拽排序 / 拖到其它分组

- 存储：`servers` 表新增 `sort_order`（增量迁移，旧库默认 0 = 原来的时间倒序）；
  `list()` 按 `sort_order ASC, created_at DESC`；新建服务器取 `MAX+1` 排到末尾。
- 后端：新命令 `apply_server_layout(items)`，整表「id + 分组 + 顺序」**单事务**落库，
  写审计（`apply_server_layout`）。
- 前端：`n-tree` 开启 `draggable`。拖到分组行 = 移入该分组末尾；拖到服务器行前/后 =
  组内排序。乐观更新，失败回滚并提示。分组自身按名称排序、不可拖动；
  搜索态禁止拖动（过滤后的子集算出来的顺序会错）。

### 7. 分组重命名

- 分组是服务器行上的字符串列，改名 = 批量改这一列：`store.rename_group()`（大小写/首尾
  空格不敏感）+ 命令 `rename_server_group`，写审计（`rename_server_group`）。
- 前端：选中分组后在详情面板点「重命名分组」；与已有分组同名等价于合并分组
  （提示受影响台数）。「默认分组」（未分组）不可改名。
- 严格分组改名时，资产分组和审批规则在同一事务里迁移。严格分组合并到普通分组后，合并后的分组要求审批。

### 8. 实时活动窗口：收起时显示最新一条 + 炫彩流光边框

- **收起（最小化）不再只剩标题**：第二行显示最新一条活动——状态标签、服务器、工具、时间；
  点标题或这一行都能展开。展开态与之前一致（最近 12 条 + 空态提示）。
- **炫彩流光边框**：容器 `padding: 1.5px` + 一个 260% 大小的 `conic-gradient` 正方形在内部
  绕中心旋转（6s 一圈），被容器的 `overflow: hidden` 裁成只有那一圈彩色边缘；内容层不透明，
  只露边。AI 有正在执行的调用时额外加一层彩色外辉光（便于余光扫到）。
- 纯 CSS 实现（无图片、无 JS 定时器、不改事件流）；`prefers-reduced-motion: reduce` 下停止
  流动但保留彩色边框。
- 验证：把同样的样式抽到临时预览页在浏览器里渲染，`getComputedStyle(.activity-rim).transform`
  在 700ms 内由 `matrix(-0.997…)` 变为 `matrix(-0.694…)`，确认 `rim-flow` 动画生效；
  收起态截图确认最新一条完整显示、边框四边可见且无接缝（渐变首尾同色）。

### 9. 双窗口常驻 + 菜单栏托盘（原主界面活动面板已移除）

主窗口一最小化或关闭，页面里的活动面板就跟着消失。所以这块能力落在**第二个 Tauri 窗口**上，
并把「App 是一个窗口」改成「App 是一套常驻服务 + 两个窗口 + 一个托盘入口」。

- **对等窗口**：`activity-float` 与主窗口同时创建、同时退出。无边框、透明、置顶、
  不进任务栏、加入所有 Space（台前调度/多桌面下不被收走）。
  启动后 900ms 亮出（等主窗口先出来），贴在主屏右下角；每次显示前重算位置。
- **主窗口最小化 / 关闭都不影响悬浮窗**：不再有「最小化才显示」的联动，也没有轮询。
- **关闭主窗口 = 收起来，不退出**：`CloseRequested` → `prevent_close()` + `hide()`。
- **菜单栏托盘**（`tauri` 的 `tray-icon` feature）：打开主界面 / 显示 AI 活动悬浮窗 /
  隐藏 AI 活动悬浮窗 / 退出 ServerDesk。**真正退出只有这里（以及 Cmd+Q）**。
- **悬浮窗内**：审批徽标点开 → `show_main_window("approvals")`（unminimize → show → focus，
  并广播 `navigate`）落到审批中心；点某条记录就地展开该次调用的命令内容。
- 主界面原来的右下角活动面板（`ActivityWidget.vue`）已删除，`App.vue` 也不再订阅活动事件。
- **鹈鹕悬浮窗**（168×118，内联 SVG）
  - 车轮、牙盘/曲柄、脚踏板、鹈鹕的两只脚**同一周期顺时针转**（1.3s；有调用在执行时 0.65s）
  - 脚踩在踏板上：踏板组以自身为中心做**反向**同角度旋转，所以脚始终水平、只跟着曲柄绕圈；
    两条腿从踏板向上伸进身体轮廓（腿根被身体盖住），看起来就是在蹬车
  - 骑行时路面滚动显示活动。有待审批时，鹈鹕单脚着地、举起听筒，车轮和踏板暂停，路面固定显示待处理数量。
  - 只要仍有一条待审批就保持接电话。最后一条批准、拒绝或超时离开队列后恢复骑行和滚动。
  - **鼠标悬停 = 加速**：`--cycle/--road/--bob` 三个 CSS 变量在 `.rider:hover` 里换挡，
    车轮/曲柄/踏板/路面/起伏同步加快（1.3s → 0.5s；执行中 0.65s → 0.34s），
    同时车尾出现速度线、车身带一圈蓝色辉光；移出即恢复。纯 CSS，无 JS 定时器
  - 左键点鹈鹕，播放一声实录叫声并张嘴。接电话时也可以点击。连续点击会从头播放同一段录音。
  - 空白区和车身可拖动窗口，右键打开原生菜单。
  - 右上角红色数字角标 = 待审批数，点它直接进审批中心
  - 点击「活动记录」可在固定尺寸的悬浮窗内查看近期记录和展开详情，按 Esc 返回鹈鹕画面。
- 右键菜单走原生 `Menu::popup`（需要 enable `tauri` 的 `unstable` feature 才能 `Manager::get_window`
  取到 `Window`），选项通过 `float-menu` 事件回到前端处理（前端掌握形态与持久化状态）。
- **持久化开关**：`meta.float_window_enabled`（缺省开启）。关掉后悬浮窗隐藏、下次启动不再
  自动出现；设置页开关、托盘「显示/隐藏 AI 活动悬浮窗」、悬浮窗的 ✕ 都走同一个
  `float::set_enabled()`，三处状态一致。
- 叫声来自美国鱼类及野生动物管理局的公共领域录音 [Baby pelican.ogg](https://commons.wikimedia.org/wiki/File:Baby_pelican.ogg)。
  应用版截取 2.76 至 3.16 秒，转为单声道并调整首尾和音量。

**交互验收**：依次加入两条待审批，先处理一条，确认鹈鹕仍接电话。清空最后一条后检查车轮恢复转动。
骑行和停车时分别点击鹈鹕，确认每次播放一声，审批角标仍能打开审批中心。

**两个 macOS 坑（都实际踩到过）**：

1. **不能用 `WebviewWindow::show()`**：它走 `makeKeyAndOrderFront:`，会激活应用并夺取键盘焦点。
   曾经在启动早期误判「应用被隐藏」而弹出悬浮窗，把锁屏输入的主密码前几个字符吃掉，
   表现为「主密码错误」（密码本身没错）。改用 `orderFrontRegardless`：只提窗口，不激活、不抢焦点。
2. **窗口操作必须回主线程**：从 tokio 工作线程调 `orderFrontRegardless` 会被 WebKit 判定
   `crashDueToApplicationCallingMainThreadOnlyWebKitAPIFromBackgroundThread` 直接 SIGKILL
   （崩溃报告里 `float::show_float ← float::show_on_start ← tokio worker`）。
   现在统一用 `float::on_main()` 把窗口操作打包进 `run_on_main_thread` 闭包并等结果。

### 10. 点开活动记录看命令内容

活动事件原来只带 `tool + server`，光看 `exec_command` 等于什么都没说。现在事件新增
`detail`：由 `activity_detail()` 按工具拼出「这条调用到底做了什么」
（`exec_command` → 命令本身；`read_file`/`log_search` → 路径与匹配串；
`write_file` → 路径 + 内容长度；`exec_batch` → 目标范围 + 命令），
统一过 `guard.sanitize`（口令/令牌/私钥片段一律 `[REDACTED]`）并按 400 字截断，
只发往本机两个 webview，不落盘、不出网。

悬浮窗里「点一行展开、再点收起」，长命令在展开区内部滚动。

## 三、验证

### 自动化

```bash
cargo test --workspace        # 86 passed（新增 store 布局/改名、guard 规则反序列化 2 个用例）
cargo check --workspace       # 通过（仅 2 条既有的 unused 变量告警）
cd apps/desktop && pnpm build # vue-tsc + vite 通过
python3 apps/desktop/scripts/check_vue_imports.py   # OK
```

新增单测：

- `store::tests::layout_reorder_and_group_rename_round_trip`：新插入排末尾、拖拽落库顺序、
  拖进未分组清空 `group_name`、改名大小写/空格不敏感。
- `guard::tests::guard_rules_accept_camel_case_and_legacy_snake_case`：前端 camelCase 能存、
  旧库 snake_case 能读、落库写 camelCase。

### 手工（打包版）

| # | 步骤 | 期待 |
|---|------|------|
| 1 | AI 用 `exec_command`（server 传 **id**）触发一次活动 | 活动窗口显示**服务器名**，不是 UUID |
| 2 | AI 用 `exec_batch` 按 group 执行 | 活动窗口显示「分组「xxx」」 |
| 3 | 触发需审批命令，保持窗口在前台 | 听到提示音（应用内或原生系统音，二者其一） |
| 4 | 设置页点「试听系统音」 | 听到系统 Glass 音 |
| 5 | 首次启动打包版 | 弹出系统通知授权框；「系统设置 → 通知」出现 ServerDesk |
| 6 | 设置页点「发送测试通知」，5 秒内切到别的应用 | 横幅出现；审计 `notify_test` 为 `accepted=true error=none` |
| 7 | 反复进出设置页 | 不再每次自动发起更新检查（启动时仍检查一次） |
| 8 | 打开只读模式 → 点「保存高级策略」→ 重启应用 → 进设置 | 只读模式仍为开启；标题显示「只读模式已生效」 |
| 9 | 只读开启时让 AI 执行 `touch /tmp/x` / `write_file` | 被硬拒并写审计（`blocked:readonly`） |
| 10 | 拖动服务器到同组另一台之前 | 松手即换位；重启应用后顺序保持 |
| 11 | 拖动服务器到另一个分组行上 | 归入该分组末尾；重启后仍在 |
| 12 | 重命名严格分组，或将它合并到普通分组 | 严格审批规则跟随改名并持久化；审计里有 `rename_server_group` |
| 13 | 等待一条 AI 调用完成，期间打开悬浮窗的「活动记录」 | 同一条记录从「执行中」更新为最终状态，点击可展开脱敏详情 |
| 14 | 修改两张安全设置卡片，只保存其中一张 | 仅保存该卡片的字段，另一张卡片保留草稿 |

## 四、涉及文件

| 文件 | 改动 |
|------|------|
| `apps/desktop/src-tauri/src/ipc.rs` | 新增 `activity_server_label()`：活动事件解析真实服务器名 |
| `apps/desktop/src-tauri/src/notify.rs` | 启动申请授权、UN 通知投递、原生提示音、测试通知/试听命令、`alert_audio_ready` |
| `apps/desktop/src-tauri/src/guard.rs` | `GuardRules` camelCase + legacy alias（+ 单测） |
| `apps/desktop/src-tauri/src/store.rs` | `sort_order` 迁移、`apply_layout` / `rename_group`（+ 单测） |
| `apps/desktop/src-tauri/src/commands.rs` | `apply_server_layout` / `rename_server_group` 命令与审计 |
| `apps/desktop/src-tauri/src/main.rs` | 注册 5 个新命令 |
| `apps/desktop/src-tauri/src/backup.rs` | 备份载荷带上 `sort_order`（旧备份缺字段按 0 处理） |
| `apps/desktop/src/alert.ts` | 持续解锁音频上下文、resume 后再排音、上报可出声状态 |
| `apps/desktop/src/components/ApprovalAlert.vue` | 改用 `armAlertSound()` |
| `apps/desktop/src/components/UpdatePrompt.vue` | 新增 `autoCheck` 属性 |
| `apps/desktop/src/views/SettingsView.vue` | 高级策略保存按钮/脏标记、试听系统音、测试通知、设置内更新实例关闭自动检查 |
| `apps/desktop/src/views/ServersView.vue` | 拖拽排序/跨组移动、分组重命名 |
| `apps/desktop/src/api/index.ts` | 新命令的 TS 封装与 `LayoutItem` 类型 |

## 五、遗留与边界

- 系统通知的**点击唤起窗口**仍由插件回调负责（`notify::init`）。macOS 原生通道的通知点击
  由系统默认行为激活应用；若后续要在原生通道上做「点击后跳到审批中心」，需要补
  `UNUserNotificationCenterDelegate`。
- 通知与提示音的**实际弹出仍受系统音量、勿扰/专注模式影响**，这部分只能在真机确认；
  本轮把「能不能发出去」变成可当场验证（测试通知 + 试听系统音）。
- 分组没有独立表，因此分组本身不能有排序/颜色等属性；改名是批量改列，空分组改名会返回
  「分组不存在」。
