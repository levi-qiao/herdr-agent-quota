# herdr-agent-quota

在 Herdr Agent 侧栏显示模型、上下文、提示词缓存用量和订阅额度。

[![CI](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml/badge.svg)](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md)

<table>
<tr><th>gauges（默认）</th><th>packed</th><th>stacked</th></tr>
<tr>
<td valign="top"><img src="docs/screenshots/sidebar-gauges.png" alt="条形布局" width="287"></td>
<td valign="top"><img src="docs/screenshots/sidebar-packed.png" alt="拼接布局" width="284"></td>
<td valign="top"><img src="docs/screenshots/sidebar-stacked.png" alt="分行布局" width="177"></td>
</tr>
</table>

插件保留 Herdr 原生的机器／工作区／标签页行、自定义样式和 worktree 分组。
带品牌色的 provider/model 行就是 agent 身份，因此不再保留灰色的原生 `agent` 行，
避免 `grok` 叠在 `Grok/grok-4.6` 上面。按额度排序和低额度通知默认关闭。
空字段自动折叠，百分比可选择显示剩余或已用额度。
默认布局是 `gauges`：在每个额度数字旁加一条进度条。进度条长度始终对应旁边打印的数字；
`cx`、`5h`、`7d`、`30d` 都跟随 `quota-percent`，整列是同一把尺子
（`packed` 和 `stacked` 的 `context N%` 仍表示已用）。标签列三个字符，内置周期
不用缩写也能对齐；服务商自定义的窗口名过长时退回 `stacked` 的普通样式，而不是截断。
cache 和 TTL 放得下就拼成一行，侧栏变窄再拆回两行。进度条按当前连接的 Herdr
endpoint 侧栏宽度定长（已计入缩进和滚动条），太窄时直接不画，不会截断数字。
调整宽度后要等下一次刷新或窗格事件才会重算；多个客户端共享同一套 pane metadata，
无法按窗口分别定长。
`gauges` 下 `cx` 行也有自己的严重程度配色，与 `5h`、`7d` 共用同一套绿/黄/红：
无论数字显示的是剩余还是已用，配色一律按剩余量分档——上下文剩余不足 50% 转黄，
不足 20% 转红。`gauges` 各行采用这套配色的低饱和版本；`packed` 和 `stacked`
仍沿用原来的颜色。

## 安装与升级

要求：**Herdr 0.9.0+**、`rust-toolchain.toml` 指定的 Rust 工具链、macOS 或 Linux，
以及受支持的 agent CLI。

```sh
git clone https://github.com/levi-qiao/herdr-agent-quota.git
cd herdr-agent-quota
./install.sh
```

只启用部分 agent：`./install.sh --agent claude,codex,omp`。
仅在需要加载新安装的 hook 或 Herdr integration 时，才需重启已经运行的 agent 会话。

在仓库目录升级：

```sh
git pull --ff-only
./install.sh
```

升级保留已有偏好，修复插件管理的配置，重新读取额度并自动恢复后台更新。
不需要删除缓存或管理 watcher 进程；Herdr 服务端连接变化后，watcher 会自动接管。

## 设置

按 `prefix+shift+q` 打开；若该快捷键已有其他用途，可运行：

```sh
herdr plugin pane open --plugin herdr-agent-quota --entrypoint settings --focus
```

<img src="docs/screenshots/settings.png" alt="Agent quota 设置" width="760">

| 设置 | 可选项 |
| --- | --- |
| Percentages | 剩余或已用比例；颜色始终表示剩余额度 |
| Layout | `gauges`（默认）在每个额度数字旁加进度条；`packed` 合并相关字段；`stacked` 将字段分行显示 |
| Row gap | Agent 之间保留零行或一行空白 |
| Watch interval | 30 秒–1 小时，默认 60 秒 |
| Fields | 主题、模型、缓存、TTL、上下文、短期／长期额度 |
| Brand colors | 开启或关闭品牌色 |
| Agent order | Herdr 默认排序，或剩余额度最少的优先 |
| Low quota alert | 关闭，或设置 1%–100% 的提醒阈值 |
| Agents | Claude、Codex、Grok、Agy、OpenCode、Pi、OMP、Devin |

方向键或空格修改，`a` 应用，`q` 关闭。脚本配置选项见 `./install.sh --help`。

## 数据来源与边界

| Agent | 额度来源 | 归属依据 |
| --- | --- | --- |
| Codex | Codex app-server；5h 和／或 7d | 插件 `CODEX_HOME` 中的当前登录 |
| Grok | CLI billing 接口；7d 或 30d | 当前 CLI 凭据 |
| Devin | CLI usage 接口；1d 和 7d | 当前 CLI 凭据 |
| Claude Code | StatusLine；5h 和 7d | 精确会话的观测 |
| Agy / Antigravity | StatusLine；5h 和 7d | 精确会话与可确认的模型额度池 |
| OpenCode | OpenCode Go usage 接口 | Go 凭据；确认的 PAYG 路由不显示订阅额度 |
| Pi | 规范 Codex collector 的额度 | 仅在记录的账号一致时复用 |
| OMP | `omp usage --json --provider <id>` | usage 账号与会话 credential pin 一致 |

额度窗口保留上游定义。模型、上下文和缓存数据优先来自已识别的会话。
`ttl≈` 表示估算的提示词缓存寿命，不保证实际过期时间。
主题提取只读取事件点名窗格的可见屏幕；内容滚走后保留已有主题。

所有受支持的工作中 agent 共用一个后台 watcher，请求间隔至少 60 秒，并在回合结束后
完成收尾刷新。OMP 另有自身的五分钟 usage 缓存。共享已确认额度来源的闲置窗格会收到同一读数。

原生 Codex、Grok、Devin collector 跟随插件的当前登录，不为每个窗格分别识别账号。
Claude/Agy 没有可靠的服务账号 ID，因此不跨会话共享观测值。
账号或模型额度池无法确认时不猜测数字。请求失败保留同一账号最后一次已确认的读数，
不会把失败解释为零用量。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| 缺少会话数据 | 运行 `herdr integration status`，安装缺失项后重启对应 agent |
| Claude/Agy 缺少额度 | 发送一轮消息，让该会话的 StatusLine 产生观测 |
| OMP 缺少额度 | 检查 `omp usage --json --redact --provider <id>` |
| Devin 缺少额度 | 检查 CLI 登录；使用自定义路径时检查 `DEVIN_CREDENTIALS_FILE` |
| 缺少侧栏行 | 运行下面的 configure action 修复插件配置 |
| packed 内容被截断 | 选择 `stacked` |
| 侧栏太窄，`gauges` 不显示进度条 | 调宽侧栏，或选择 `stacked` |
| 调整宽度后 `gauges` 仍是旧长度 | 用 `prefix+shift+r` 刷新；没有随拖动实时发布的路径 |
| `gauges` 下 cache 和 TTL 仍分两行 | 把侧栏加宽到放得下 `cache … · ttl≈…` |

```sh
herdr plugin action invoke refresh --plugin herdr-agent-quota
herdr plugin action invoke configure --plugin herdr-agent-quota
```

完整卸载使用 `./uninstall.sh`，只移除部分 agent 使用 `./uninstall.sh --agent grok`。
配置修改可恢复，用户自己的设置与其他 agent 不受影响。

## 参与开发

开发与验证见 [CONTRIBUTING.md](CONTRIBUTING.md)，数据处理及漏洞报告见
[SECURITY.md](SECURITY.md)，版本变更见 [CHANGELOG.md](CHANGELOG.md)。
历史调研索引见 [docs/README.md](docs/README.md)。

## 许可证

[MIT](LICENSE)。本项目与 Herdr 及受支持的 AI 供应商无隶属关系。
