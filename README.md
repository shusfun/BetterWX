# <img width="300" src="logo_word.png" />
某补丁工具

## Windows 图形界面

新增 Tauri 桌面版，专注防撤回。`BetterWX.exe` 为免安装单文件，使用系统 WebView2；运行时缺失时提供原生安装窗口。更新中心从 `shusfun/BetterWX` 的公开稳定 Release 获取签名更新，下载支持断点续传与最多 4 连接并发。

开启或关闭前请完全退出微信。工具将原版备份保存在 BetterWX 用户数据目录，关闭工具后补丁仍然生效。微信更新后应重新检查；“补丁可匹配”不等同于真实撤回效果已经验证。

桌面版构建：安装 Node.js 22、pnpm 10、Visual Studio C++ 构建工具后执行：

```sh
pnpm install --frozen-lockfile
pnpm desktop:build
```

将 `target/release/BetterWX-Desktop.exe` 重命名为 `BetterWX.exe` 即可分发。原命令行版仍使用 `cargo build --release -p betterwx` 构建。

发布时版本号须同步更新根目录与 desktop 的 Cargo.toml、package.json、desktop/tauri.conf.json，并使用 `v<版本>` 标签。CI 创建草稿 Release，公开发布后客户端才会发现更新。发布环境须配置 `BETTERWX_UPDATE_PRIVATE_KEY`（Ed25519 PKCS#8 PEM）Secret，与 `desktop/update-public-key.txt` 的公钥对应；缺少密钥或不匹配会拒绝签名发布。私钥不应提交到仓库。自行维护分支时，可使用 `scripts/generate-update-key.mjs` 生成自己的密钥对，并同步修改更新源配置。

未完成下载存放在 `%LOCALAPPDATA%/BetterWX/Downloads`，可从更新中心清除；应用界面缓存位于本次会话的临时目录，退出后清理。WebView2 为系统共享运行时，安装后保留。

支持平台：Windows x64<br>
支持版本：**4.2.0.13**（已对本机该版本文件完成扫描）

## 功能

- 自动登录
- 登录失败也保留登录信息
- 防撤回
- 共存
- 接入系统通知
- 调整时间格式
- 逐条显示时间
- 表情复制为图片
- 不要只读保存
- 防止文件过期
- 使用系统浏览器打开网页
- 关闭内置浏览器
- 无障碍读取控件
- 兼容外部工具
- 关闭托盘闪烁
- 自定义托盘图标
- 自定义提示音

## 用法
[前往发行版](https://github.com/shusfun/BetterWX/releases)下载并运行 `betterwx`，选择版本启用需要的功能。

## 构建

```sh
cargo build --release
```

## 问题解答

- 报错了？补丁失效了？<br>
  可以来[议题](https://github.com/shusfun/BetterWX/issues)中反馈。

- 旧版本能用吗？<br>
  不包能用，更新只考虑最新版，更新时间随机。

- 本项目的许可证？<br>
  Unlicense 不在乎任何权利。

- 作者可爱吗？<br>
  很可爱的 owo，要不要来看看[我做的其他东西](https://github.com/zetaloop)？
