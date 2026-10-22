# 自定义主题

ltop 使用与 btop 兼容的主题文件——任何 btop 主题可以直接使用,也可以自己写,
包括只修两三行、解决「终端配色下某些字看不清」的最小文件。

## 主题文件的位置

- 内嵌:41 个从 btop 转换的主题,主题菜单(`m` → theme)选择,或命令行
  `--theme <名称>`。
- Omarchy:若存在 `~/.local/state/omarchy/current/theme/btop.theme`,菜单会
  额外提供 `omarchy` 项——选中后跟随桌面主题,桌面换主题时热重载。
- **自定义**:把 `.theme` 文件放进 `~/.config/ltop/themes/`,或直接加载任意
  文件:

```bash
ltop --theme ~/my-colors.theme
```

`--theme` 直载的主题是固定的(不跟随 omarchy)。

## 文件格式

键值对,一行一个;`#` 开头的行是注释。颜色支持 `#RRGGBB`、短写 `#RGB`、
两位灰度简写(`#80` = 中灰);`main_bg` 还支持**显式留空 = 终端默认色**
(透明背景,对使用终端透明效果的用户重要):

```
theme[main_fg] = "#c0caf5"
theme[inactive_fg] = "#565f89"
```

## 变量速查

| 键 | 控制范围 |
|---|---|
| `main_bg` | 弹窗/对话框底色;留空 = 终端默认(透明) |
| `main_fg` | **正文**——表格单元格、详情值、help 描述 |
| `inactive_fg` | **弱化字**——标签、提示、次级信息、时间戳 |
| `hi_fg` | 高亮:盒边框、快捷键、组标题、状态徽章 |
| `title` | 弹窗/盒子标题 |
| `graph_text` | 强调色:进程名、UDP 徽章、重点信息 |
| `net_box` | NET 盒边框 |
| `proc_box` | LAN 盒边框(btop 键名,兼容保留) |
| `mem_box` | DISK 盒边框(btop 键名,兼容保留) |
| `download_start/mid/end` | 下载渐变:图表、↓ 速率、ESTAB 徽章 |
| `upload_start/mid/end` | 上传渐变:图表、↑ 速率 |
| `used_start/mid/end` | 磁盘已用容量色 |
| `temp_start/mid/end` | 告警渐变 |

渐变必须三段齐全(start/mid/end),缺任何一段则该渐变被忽略、保持内嵌默认。

## 最小示例

只覆盖看不清的键,其余回落到当前选中的内嵌主题:

```
theme[main_fg] = "#e0e0e0"
theme[inactive_fg] = "#909090"
```

保存为 `~/.config/ltop/themes/readable.theme`,主题菜单选 `readable`
(或 `ltop --theme ~/.config/ltop/themes/readable.theme`)。

## 完整示例

最省事的完整主题是拷一个内嵌 btop 主题来改:

```bash
cp /usr/share/btop/themes/dracula.theme ~/.config/ltop/themes/my.theme
```

完整文件的形状参考(Dracula 配色):

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

macOS 上 LAN 盒的 MAC 列无论什么主题都不可见——系统对非 Apple 签名的
二进制做了脱敏。见主 README 的 macOS 说明。
