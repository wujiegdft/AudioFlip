# AudioFlip

Windows 11 托盘音频输出切换工具（Rust + Win32，单个 exe，无依赖）。

## 用法

- **右键**托盘图标：勾选参与轮换的设备（白名单），开关开机自启，打开配置文件夹，退出
- **左键**单击：按白名单顺序切换默认输出设备（同时设置 Console / Multimedia / Communications 三个角色）
- 托盘图标 = 当前设备的系统图标；悬浮提示显示设备名
- 白名单可用设备少于 2 个时，左键直接弹出菜单

建议在 设置 → 个性化 → 任务栏 → 其他系统托盘图标 中开启 AudioFlip，让图标常驻任务栏。

## 配置

`%APPDATA%\AudioFlip\config.toml`，轮换顺序 = 文件中的顺序。按 `id` 匹配，`id` 失效时按 `name` 回退匹配并自动更新 `id`。

## 构建

```
cargo build --release      # target\release\audioflip.exe
cargo test
cargo run --example probe  # 列出输出设备（* 为当前默认）
```
