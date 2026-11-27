# ServerDesk 交互约定

## 业务来源

- [安全边界、AI 审批与审计](../README.md)
- [服务器资产、保险库与备份方案](plan.md)
- [审批与通知](r6-acceptance.md)
- [资产分组与排序](ui-tree-groups.md)

## Canonical UI Map

| Capability | Canonical owner | Source of truth | Allowed variants | Verification |
| --- | --- | --- | --- | --- |
| Select/Listbox | Naive UI NSelect | 各页面字段语义 | 单选、多选、标签 | 键盘选择、弹层 |
| Form | NForm、NFormItem 与 NInput | 现有字段与 Tauri API | 资产新增、编辑，保险库原生提交 | 校验失败与重试 |
| Scrollbar | styles.css | DESIGN.md | 主窗口、透明悬浮窗 | 最小窗口与计算样式 |
| Toast | Naive UI NMessageProvider | 操作返回结果 | 成功、信息、错误 | 成功与失败路径 |
| CRUD | servers store 与 Tauri API | 资产契约、现有保存和删除接口 | 列表选中最近保存的资产 | 搜索、编辑、排序与删除回归 |

## 导航与反馈

- 快捷搜索使用 Cmd/Ctrl + K，支持方向键、Enter 和 Escape。输入法组词期间保留输入法行为。快捷搜索可直接定位服务器或页面。
- 服务器筛选、选中项与分组折叠状态保存在当前解锁会话。审计筛选与分页保留在当前会话。保险库锁定时销毁页面缓存与快捷搜索结果。
- 服务器状态来自 SSH 连接池和当前会话的操作结果。连接中允许取消，失败后可以重试。过滤期间暂停拖拽与组内排序，正常浏览时可通过「更多」菜单用键盘调整组内顺序。
- 审批按到期时间排序。请求提交期间与到期后禁用操作按钮。审批成功提示说明决定已提交，实际执行结果从审计确认。审批内容沿用后端提供的脱敏文本。
- 审计通过服务端筛选、分页。详情抽屉关闭后焦点回到触发按钮。过滤变化回到第一页，过期查询结果被丢弃。
- 备份手动恢复以合并为默认。覆盖恢复需要二次确认，说明会清空的资产与主机指纹。
- 列表加载失败保留可用上下文并给出刷新入口。页面切换、状态反馈和系统减少动态效果遵循[设计规范](DESIGN.md)。
