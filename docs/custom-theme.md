# Custom themes

ltop uses btop-compatible theme files. Any btop theme works as-is, and you
can write your own — including a two-line file that fixes just the colours
you cannot read on your terminal.

## Where themes live

- Built-in: 72 themes converted from btop, selectable from the theme menu
  (`m` → theme) or `--theme <name>` on the command line.
- Omarchy: if `~/.local/state/omarchy/current/theme/btop.theme` exists, the
  menu also offers `omarchy` — selecting it follows the desktop theme and
  live-reloads when the desktop theme changes.
- **Custom:** put a `.theme` file in `~/.config/ltop/themes/`, or load any
  file directly:

```bash
ltop --theme ~/my-colors.theme
```

A theme loaded with `--theme` is pinned (it does not follow omarchy).

## File format

Key–value pairs, one per line; `#` lines are comments. Colours accept
`#RRGGBB`, short `#RGB`, a two-digit grayscale shorthand (`#80` = mid grey),
and — for `main_bg` only — an empty value meaning "terminal default"
(transparent background, important for terminal transparency users):

```
theme[main_fg] = "#c0caf5"
theme[inactive_fg] = "#565f89"
```

## Variable reference

| Key | Controls |
|---|---|
| `main_bg` | popup/dialog background; leave empty for terminal default |
| `main_fg` | **body text** — table cells, detail values, help descriptions |
| `inactive_fg` | **dimmed text** — labels, hints, secondary info, timestamps |
| `hi_fg` | highlights: box borders, keybindings, group titles, badges |
| `title` | popup/box titles |
| `graph_text` | accent: process names, UDP badge, emphasis |
| `net_box` | NET box border |
| `proc_box` | LAN box border (btop key name kept for compatibility) |
| `mem_box` | DISK box border (btop key name kept for compatibility) |
| `download_start/mid/end` | download gradient: charts, rx rates, ESTAB badge |
| `upload_start/mid/end` | upload gradient: charts, tx rates |
| `used_start/mid/end` | disk usage colours |
| `temp_start/mid/end` | alert gradient |

Gradients must define all three stops (start/mid/end); a gradient missing
any stop is ignored and the built-in default stays.

## Minimal example

Only override what you cannot read — everything else falls back to the
currently selected built-in theme:

```
theme[main_fg] = "#e0e0e0"
theme[inactive_fg] = "#909090"
```

Save as `~/.config/ltop/themes/readable.theme`, then pick `readable` from
the theme menu (or `ltop --theme ~/.config/ltop/themes/readable.theme`).

## Full example

The easiest full theme is copying a built-in btop theme and editing:

```bash
cp /usr/share/btop/themes/dracula.theme ~/.config/ltop/themes/my.theme
```

For reference, this is the shape of a complete file (Dracula palette):

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

## Known limitation

The neighbour (LAN) box's MAC column is hidden on macOS regardless of
theme — the system sanitizes it for non-Apple-signed binaries. See the
macOS notes in the main README.
