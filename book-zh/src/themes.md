# 主题

ltop 读 **btop 兼容的主题文件**: 任何 btop 主题直接可用，自己写也行——哪怕
只是两行，修掉终端上读不清的那几个颜色。

![Omarchy 主题跟随——桌面切主题，ltop 实时变色](../../assets/screenshots/omarchy-theme-follow.gif)

## 主题放哪

- **内嵌**: 72 套从 btop 转换的主题，主题菜单（`m` → theme）或命令行
  `--theme <name>` 选择。
- **Omarchy**: 若存在 `~/.local/state/omarchy/current/theme/btop.theme`，
  菜单会多出 `omarchy` 条目——选中即跟随桌面主题，桌面换主题时热重载。
- **`--follow-theme`:** 启动时读到的是*当时的*快照；加 `--follow-theme`
  则桌面主题变化时**运行中的** ltop 也跟着变色。默认关（它要轮询主题
  文件，想要上面 GIF 的效果再开）。
- **自定义**: `.theme` 文件放进 `~/.config/ltop/themes/`，或直接加载:

```bash
ltop --theme ~/my-colors.theme
```

`--theme` 加载的主题是钉死的——不跟随 omarchy。

## 文件格式

每行一个键值对，`#` 开头是注释。颜色接受 `#RRGGBB`、缩写 `#RGB`、两位
灰度缩写（`#80` = 中灰），`main_bg` 还接受空值，含义是"终端默认色"
（透明背景，终端半透明用户需要）:

```
theme[main_fg] = "#c0caf5"
theme[inactive_fg] = "#565f89"
```

## 变量参考

| 键 | 控制什么 |
|---|---|
| `main_bg` | 弹窗/对话框背景；留空 = 终端默认 |
| `main_fg` | **正文**——表格单元格、详情值、帮助描述 |
| `inactive_fg` | **弱化文本**——标签、提示、次要信息、时间戳 |
| `hi_fg` | 高亮: 盒子边框、键位、组标题、徽章 |
| `title` | 弹窗/盒子标题 |
| `graph_text` | 强调色: 进程名、UDP 徽章、着重 |
| `net_box` | NET 盒边框 |
| `proc_box` | LAN 盒边框（键名沿用 btop，保兼容） |
| `mem_box` | DISK 盒边框（键名沿用 btop，保兼容） |
| `download_start/mid/end` | 下载渐变: 图表、rx 速率、ESTAB 徽章 |
| `upload_start/mid/end` | 上传渐变: 图表、tx 速率 |
| `used_start/mid/end` | 磁盘用量色 |
| `temp_start/mid/end` | 告警渐变 |

渐变必须三档齐全（start/mid/end）；缺任何一档整条渐变被忽略，用内建默认。

## 最小示例

只覆盖读不清的颜色，其余回落到当前选中的内建主题:

```
theme[main_fg] = "#e0e0e0"
theme[inactive_fg] = "#909090"
```

存为 `~/.config/ltop/themes/readable.theme`，主题菜单里选 `readable` 即可。

## 完整示例

最省事的完整主题是拷一份内建 btop 主题来改:

```bash
cp /usr/share/btop/themes/dracula.theme ~/.config/ltop/themes/my.theme
```

完整文件长这样（Dracula 配色，供参考）:

```
theme[main_bg] = "#282a36"
theme[main_fg] = "#f8f8f2"
theme[title] = "#bd93f9"
theme[hi_fg] = "#8be9fd"
theme[inactive_fg] = "#6272a4"
theme[graph_text] = "#ff79c6"
theme[net_box] = "#bd93f9"
theme[proc_box] = "#ff79c6"
theme[mem_box] = "#8be9fd"
theme[download_start] = "#8be9fd"
theme[download_mid] = "#8be9fd"
theme[download_end] = "#57c7ff"
theme[upload_start] = "#ff79c6"
theme[upload_mid] = "#ff79c6"
theme[upload_end] = "#bd93f9"
theme[used_start] = "#50fa7b"
theme[used_mid] = "#f1fa8c"
theme[used_end] = "#ff5555"
theme[temp_start] = "#50fa7b"
theme[temp_mid] = "#f1fa8c"
theme[temp_end] = "#ff5555"
```

## 已知限制

LAN 盒的 MAC 列在 macOS 上无论主题如何都隐藏——系统对非 Apple 签名二进制
做了脱敏。见[兼容性](compat.md#macos)。
