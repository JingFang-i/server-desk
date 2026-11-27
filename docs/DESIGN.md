---
version: alpha
name: ServerDesk
description: 深色桌面运维工作台，围绕服务器、审批和审计组织日常操作。
colors:
  background: "#121923"
  sidebar: "#101620"
  surface: "#1b2634"
  popover: "#222f40"
  border: "#324254"
  text: "#eaf1f8"
  muted: "#a3b3c5"
  primary: "#73c7ff"
  primary-hover: "#a5dcff"
  primary-pressed: "#55afea"
  success: "#71d4ab"
  warning: "#f5c279"
  danger: "#ff9b9a"
typography:
  sans:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Noto Sans CJK SC", sans-serif'
  mono:
    fontFamily: "ui-monospace, Menlo, Consolas, monospace"
rounded:
  control: "7px"
  panel: "12px"
spacing:
  page-padding: "24px"
  panel-gap: "14px"
---

# ServerDesk 设计规范

## Overview

服务器资产管理员在本地窗口完成连接、文件传输、AI 审批和审计。主窗口采用深石板蓝工作台，简体中文字采用系统字体，保留 ServerDesk 原有图标与独立活动悬浮窗。主窗口的特征是一条跟随真实 AI 活动变化的底部轨道。

运行时以 [styles.css](../apps/desktop/src/styles.css) 为准。背景、面板、浮层、文字、强调色和状态色分别对应 `--canvas`、`--surface`、`--popover`、`--text`、`--accent`、`--success`、`--warning` 与 `--danger`。App 从这些变量读取 Naive UI 主题值。Tauri 启动窗口的底色与 CSS 的 `--canvas` 对齐。

## Colors

冰蓝用于主操作、选中和焦点。绿色表示已连接或成功，琥珀色表示待处理，红色表示失败或危险操作。状态都带可识别的文字。工具区与数据面板用色调和细边框区分。

## Typography

页面标题承载导航层级。主机地址、命令、活动标识与倒计时使用等宽字体。长名称和备注允许折行，表格里的长内容可以在详情里完整阅读。

## Layout

应用壳按侧栏、页头、工作区和底部活动轨道布局。窗口宽度不超过 1020 px 时，侧栏收为图标列。900 × 600 px 仍需显示连接与审批入口。服务器资产树和详情独立滚动，设置和备份随工作区滚动。

## Elevation & Depth

内容面板使用边框和色调区分。弹层沿用 Naive UI 的遮罩与焦点处理。锁屏用静态光晕标出解锁入口。

## Shapes

面板使用 12 px 圆角，主要控件使用 7 px 圆角。状态点与计数角标保留圆形语义。

## Components

Naive UI 提供输入、下拉、消息、对话框、表格和抽屉。普通动作使用标准按钮，当前主操作突出显示。悬停和键盘聚焦都可辨识。连接和批准依赖实际返回值。危险确认写清目标与后果。

图标使用 Ionicons 线性样式。折叠导航保留可读名称和提示。活动轨道的状态转换、选中标记和连接成功反馈在短时内完成。动画遵循系统减少动态效果设置，空闲的活动轨道保持静止。

## Do's and Don'ts

- 优先把当前任务下一步放在用户看得到的位置。
- 用文本说明状态与结果，颜色和动效用来辅助识别。
- 将视觉表达集中在状态变化上，运行中的长列表保持稳定。
