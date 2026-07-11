# Configuration

rwl is configured with a single Lua file at **`~/.config/rwl/config.lua`**
(`$HOME/.config/rwl/config.lua`; `/root/.config/rwl/config.lua` when `HOME` is
unset). It is read at startup and re-read on the `reload_config` action.

Key points:

- **Everything is optional.** Each field falls back to a compiled-in default
  when the file, its table, or a specific key is absent or the wrong type. An
  empty file (or no file at all) yields a fully working compositor.
- **Unknown keys are reported, not fatal.** Misspelled top-level or sub-table
  keys are logged (`Unknown config keys: …`) and surfaced through the
  config-error channel, but loading continues with defaults for the rest.
- **A parse error falls back to full defaults** and the error is logged and
  printed to stderr.
- The config runs in a **sandboxed-ish Lua 5.4 VM** with the standard library
  available (`string`, `table`, `math`, `io`, `os`, `require`, …). When the
  `hooks` feature is enabled, the VM is kept alive so callbacks can fire on
  events (see [Lua hooks](#lua-hooks)).

The recognized keys are validated against the tables in
[`src/config/lua.rs`](../src/config/lua.rs); the defaults live in
[`src/config/types.rs`](../src/config/types.rs) and
[`src/config/mod.rs`](../src/config/mod.rs).

---

## Schema

Configuration is organised into top-level scalars and a handful of grouped
tables. The table below lists every recognized key, its group, and its default.

### Top-level

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `tag_count` | int | `6` | Number of tags. |
| `rules` | table[] | see [Rules](#rules) | Window rules. |
| `layouts` | table[] | tile, monocle, col, scroll, dwindle, bstack, centeredmaster | Available layouts (order defines `set_layout` indices). |
| `monitor_rules` | table[] | one default rule | Per-output settings. |
| `keys` | table[] | see [Default keybindings](#default-keybindings) | Keyboard bindings. |
| `buttons` | table[] | Move / ToggleFloating / Resize on `M`+button | Pointer bindings. |
| `auto_spawn` | table | `chromium` on tag 1, `foot` on tag 2 | Command auto-spawned the first time a tag is viewed. |
| `startup_cmds` | table[] | *(empty)* | Commands run once at startup (`startup-cmds` feature). |
| `bar_cmd` | string[] | *(none)* | External status-bar command; receives the IPC status stream on stdin. |
| `pertag_layouts` | table | tile on all tags | Per-tag layout assignment (`pertag-layouts` feature). |

Grouped tables: `windows`, `effects`, `keyboard`, `mouse`, `bar`, `overview`,
`pip`, plus `rwl` (the hooks namespace). Each is described below.

### `windows`

| Key | Type | Default | Feature |
|-----|------|---------|---------|
| `follow` | bool | `true` | Focus follows a window moved to another monitor/tag. |
| `border_px` | int | `1` | Border width in pixels. |
| `border_color` | color | `#222222ff` | Unfocused border colour. |
| `focus_color` | color | `#ffffffff` | Focused border colour. |
| `fullscreen_bg` | color | black | Backdrop behind fullscreen windows. |
| `corner_radius` | int | `8` | Rounded-corner radius. | `rounded-corners` |
| `gaps_px` | int | `0` | Gap between tiled windows. | `gaps` |
| `auto_back_empty_tag` | bool | `true` | Return to previous tag when a tag empties. | `auto-back-empty-tag` |

### `effects`

| Key | Type | Default | Feature |
|-----|------|---------|---------|
| `fade_in_ms` | int | `0` | Window fade-in duration (0 = off). | `fade` |
| `fade_out_ms` | int | `0` | Window fade-out duration (0 = off). | `fade` |
| `tag_transition` | bool | `true` | Slide animation on tag switch. | `tag-transition` |
| `tag_transition_ms` | int | `250` | Slide duration. | `tag-transition` |

### `keyboard`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `xkb_layout` | string | `"pt"` | XKB layout. |
| `xkb_rules` | string | *(system)* | XKB rules. |
| `xkb_model` | string | *(system)* | XKB model. |
| `xkb_variant` | string | *(none)* | XKB variant. |
| `xkb_options` | string | *(none)* | XKB options (e.g. `"ctrl:nocaps"`). |
| `numlock` | bool | `true` | Enable NumLock at startup. |
| `capslock` | bool | `false` | Enable CapsLock at startup. |
| `repeat_rate` | int | `25` | Key repeats per second. |
| `repeat_delay` | int | `600` | Delay (ms) before repeat starts. |

### `mouse`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `mouse_focus` | bool | `true` | Focus follows pointer. |
| `warp_cursor` | bool | `true` | Warp pointer to focused window (`warp` feature). |
| `tap_to_click` | bool | `true` | Touchpad tap-to-click. |
| `tap_and_drag` | bool | `true` | Tap-and-drag. |
| `drag_lock` | bool | `true` | Drag lock. |
| `natural_scrolling` | bool | `true` | Natural (reversed) scrolling. |
| `disable_while_typing` | bool | `true` | Disable touchpad while typing. |
| `left_handed` | bool | `false` | Left-handed button mapping. |
| `middle_button_emulation` | bool | `false` | Emulate middle click. |
| `scroll_method` | enum | `two_finger` | `no_scroll` / `two_finger` / `edge` / `button`. |
| `click_method` | enum | `button_areas` | `none` / `button_areas` / `clickfinger`. |
| `accel_profile` | enum | `adaptive` | `flat` / `adaptive`. |
| `accel_speed` | float | `0.0` | Pointer acceleration (`-1.0 .. 1.0`). |
| `tap_button_map` | enum | `lrm` | Tap button mapping. |
| `cursor_timeout` | int | `5` | Seconds of inactivity before the cursor hides. |
| `cursor_theme` | string | *(system)* | XCursor theme name. |
| `cursor_size` | int | `24` | Cursor size. |

Enum values are parsed leniently by [`src/config/lua.rs`](../src/config/lua.rs);
see that file for the exact accepted spellings.

### Colours

Colours accept either a `"#rrggbb"` / `"#rrggbbaa"` string or a packed
`0xRRGGBBAA` integer. Internally they become `[f32; 4]` RGBA in `0.0..1.0`
(`hex_color()` in [`src/config/types.rs`](../src/config/types.rs)).

### Rules

Each entry in `rules` matches a window by app-id and/or title and applies
placement. Fields (defaults in parentheses):

| Field | Type | Meaning |
|-------|------|---------|
| `id` | string | Match on app-id / class. |
| `title` | string | Match on window title. |
| `tags` | int (mask) | Tag(s) to assign (`0` = keep current). |
| `switch_to_tag` | bool | View the assigned tag when the window opens. |
| `is_floating` | bool | Open floating. |
| `monitor` | int | Target monitor index (`-1` = current). |
| `scratch_key` | char | Bind the window to a scratchpad key (`scratchpad` feature). |

Default rules place `chromium-browser` on tag 1 (switching to it), `foot` on
tag 2 (switching to it), and register a `scratchpad`-app-id floating scratchpad
on key `s`.

### Layouts

`layouts` is an ordered list of `{ symbol = "...", kind = "..." }`. The order
determines the index used by the `set_layout` action. Built-in kinds (each a
Cargo feature): `tile` (`[]=`), `monocle` (`[M]`), `col` (`||`), `scroll`
(`>>>`), `dwindle` (`[@]`), `bstack` (`(B)`), `centeredmaster` (`|-|`). See
[FEATURES.md](FEATURES.md#tiling-layouts) for how each arranges windows.

### Monitor rules

Each `monitor_rules` entry (matched by `name`, or the default rule when unnamed):

| Field | Default | Meaning |
|-------|---------|---------|
| `name` | *(none)* | Output name to match (e.g. `"eDP-1"`). |
| `mfact` | `0.55` | Master-area fraction. |
| `nmaster` | `1` | Number of master windows. |
| `scale` | `1.0` | Output scale. |
| `layout_idx` | `0` | Starting layout index. |
| `transform` | `normal` | Output transform / rotation. |
| `x`, `y` | `-1` | Output position (`-1` = auto-place). |

### `pertag_layouts` (`pertag-layouts` feature)

Assign layouts per tag, either fixed or chosen by tiled-window count:

```lua
pertag_layouts = {
  [1] = "monocle",                      -- always monocle on tag 1
  [2] = { "monocle", "scroll", "col" }, -- 1 win → monocle, 2 → scroll, 3+ → col
}
```

Fixed rules apply on tag switch; count-based rules re-evaluate as windows open
and close. A manual layout change on a tag pins it and suppresses automatic
switching for that tag.

---

## Bindings

### Binding syntax

```lua
keys = {
  { mods = "M",  key = "Return", action = "spawn", cmd = { "foot" } },
  { mods = "MS", key = "q",      action = "quit" },
}

buttons = {
  { mods = "M", button = "left",  action = "move" },
  { mods = "M", button = "right", action = "resize" },
}
```

**Modifiers** (`mods`) are a string of letters, combined by concatenation:

| Letter | Modifier |
|--------|----------|
| `M` | Super / logo (the default "mod key") |
| `S` | Shift |
| `C` | Control |
| `A` | Alt |

**Keys** (`key`) are XKB keysym names. A single printable ASCII character maps
directly (`"a"`, `"1"`, `"+"`); named keysyms use the table in
[`src/config/keybinds.rs`](../src/config/keybinds.rs) (`Return`, `Escape`,
`Tab`, `Up`/`Down`/`Left`/`Right`, `space`, `grave`, `F1`–`F12`,
`XF86Audio*`, `XF86MonBrightness*`, `XF86Switch_VT_*`, …). An **empty** `key = ""`
is the mod-tap gesture (see below).

**Buttons** (`button`): `"left"`, `"right"`, `"middle"`, or a raw evdev code.

### Actions

Every action name accepted in a binding (from `parse_action` in
[`src/config/keybinds.rs`](../src/config/keybinds.rs)):

| Action | Extra fields | Effect |
|--------|--------------|--------|
| `spawn` | `cmd = {…}` | Launch a command. |
| `focus_stack` | `dir` (±1) | Move focus up/down the stack. |
| `inc_nmaster` | `delta` | Change master-window count. |
| `set_mfact` | `delta` | Change master-area fraction. |
| `zoom` | | Swap focused window with master. |
| `view_prev` | | View the previously selected tag. |
| `view_next_occ_tag` | `dir` (±1) | Jump to the next/prev occupied tag. |
| `kill_client` | | Close the focused window. |
| `set_layout` | `index` | Select layout by index (`usize::MAX` = last/toggle). |
| `cycle_layout` | | Cycle to the next layout. |
| `toggle_floating` | | Toggle floating on the focused window. |
| `toggle_fullscreen` | | Toggle fullscreen. |
| `toggle_passthrough` | | Toggle keybinding passthrough (keys go to the client). |
| `toggle_gaps` | | Toggle gaps. (`gaps`) |
| `view` | `mask` | View the given tag mask. |
| `view_tag_spawn` | `mask` | View a tag, auto-spawning its `auto_spawn` command on first view. |
| `toggle_view` | `mask` | Toggle a tag into/out of the current view. |
| `tag` | `mask` | Move the focused window to the tag mask. |
| `toggle_tag` | `mask` | Toggle a tag on the focused window. |
| `focus_mon` | `dir` | Focus the next/prev monitor. |
| `tag_mon` | `dir` | Send the focused window to the next/prev monitor. |
| `move` | | Interactive move (pointer binding). |
| `resize` | | Interactive resize (pointer binding). |
| `toggle_scratch` | `scratch_key`, `cmd` | Toggle a scratchpad. (`scratchpad`) |
| `focus_or_toggle_scratch` | `scratch_key`, `cmd` | Focus the scratchpad, or toggle if already focused. (`scratchpad`) |
| `focus_or_toggle_matching_scratch` | `scratch_key`, `cmd` | Same, matched by app-id. (`scratchpad`) |
| `chvt` | `vt` | Switch virtual terminal. |
| `reload_config` | | Re-read `config.lua`. |
| `quit` | | Exit the compositor. |
| `toggle_bar` | | Show/hide the bar. (`bar`) |
| `bar_prompt` | | Open the bar prompt. (`bar`) |
| `toggle_overview` | | Toggle current-tag overview. (`overview`) |
| `toggle_overview_all` | | Toggle all-tags overview / mission control. (`overview`) |
| `toggle_pip` | | Toggle picture-in-picture on the focused window. (`pip`) |
| `move_pip` | | Move the PiP thumbnail to the next corner. (`pip`) |
| `lock` | | Engage the native screen locker. (`lock`) |

### Default keybindings

The compiled-in defaults (from `default_keys()` in
[`src/config/keybinds.rs`](../src/config/keybinds.rs)). `M` = Super.

| Binding | Action |
|---------|--------|
| `M`+`Return` | Spawn `foot` (terminal) |
| `M`+`Shift`+`Return` | Spawn `wmenu-run` (launcher) |
| `M`+`↓` / `M`+`→` | Focus next in stack |
| `M`+`↑` / `M`+`←` | Focus previous in stack |
| `M`+`i` / `M`+`d` | Increase / decrease `nmaster` |
| `M`+`Ctrl`+`+` / `M`+`Ctrl`+`-` | Shrink / grow master area |
| `M`+`Escape` / `M`+`Shift`+`Escape` | Next / previous occupied tag |
| `M`+`z` | Zoom (swap with master) |
| `M`+`\` | View previous tag |
| `M`+`q` | Close focused window |
| `M`+`t` / `f` / `o` / `c` | Set layout 0 / 1 / 2 / 3 |
| `M`+`a` | Set last layout (`usize::MAX`) |
| `M`+`Tab` | Cycle layout |
| `M`+`Shift`+`Space` | Toggle floating |
| `M`+`e` | Toggle fullscreen |
| `M`+`0` | View all tags |
| `M`+`Shift`+`=` | Tag focused window to all tags |
| `M`+`,` | Focus previous monitor |
| `M`+`Ctrl`+`<` / `>` | Send window to previous / next monitor |
| `M`+`1` / `2` | View tag 1 / 2 (auto-spawn on first view) |
| `M`+`3`…`6` | View tag 3…6 |
| `M`+`Ctrl`+`N` | Toggle tag N into view |
| `M`+`Shift`+`<sym>` | Move window to tag N |
| `M`+`Ctrl`+`Shift`+`<sym>` | Toggle tag N on window |
| `M`+`r` | Reload config |
| `M`+`x` | Toggle passthrough |
| `M`+`Shift`+`Q` | Quit |
| `Ctrl`+`Alt`+`Backspace` | Quit |
| `Ctrl`+`Alt`+`F1`…`F12` | Switch VT 1…12 |
| `M`+`<` / `M`+`h` / `M`+`k` | Scratchpad toggle / focus-or-toggle / matching (`scratchpad`) |
| `M`+`` ` `` / `M`+`Shift`+`` ` `` | Overview / all-tags overview (`overview`) |
| tap `M` alone | Toggle overview (`overview`+`mod-tap`) |
| `M`+`p` / `M`+`Shift`+`p` | Toggle / move PiP (`pip`) |
| `M`+`Shift`+`L` | Native lock (`lock`) |

Default pointer bindings: `M`+Left = move, `M`+Right = resize,
`M`+Middle = toggle floating.

---

## Feature settings blocks

### `bar` (`bar` feature)

The embedded status bar. Recognized keys (from `BAR_KEYS` in
[`src/config/lua.rs`](../src/config/lua.rs)): `font`, `vertical_padding`,
`buffer_scale`, `hidden`, `bottom`, `hide_vacant`, `status_commands`,
`center_title`, `active_color_title`, colour pairs `active_fg`/`active_bg`,
`occupied_fg`/`occupied_bg`, `inactive_fg`/`inactive_bg`, `urgent_fg`/`urgent_bg`,
`middle_bg`, `middle_bg_selected`, `blocks`, `blocks_delim`, `tag_names`.
Defaults live in `crate::features::bar::BarSettings`.

### `overview` (`overview` feature)

`hint_keys` (letters used for jump hints), `dim`, `padding`, `anim_ms`,
`highlight_color`, `highlight_px`, `border_color`, `border_px`. Defaults in
`crate::features::overview::OverviewSettings`.

### `pip` (`pip` feature)

`width`, `margin`, `border_px`, `border_color`, `corner`. Defaults in
`crate::features::pip::PipSettings`.

---

## Lua hooks

With the `hooks` feature enabled, the config VM stays alive after loading and
user-defined callbacks fire on compositor events. Define any of:

```lua
function on_window_open(win)  end   -- window just entered the layout
function on_window_close(win) end   -- window just left the layout
function on_tag_switch(old, new) end
function on_focus(win) end
function on_title_change(win, title) end
```

Example — dynamic layout that switches tag 2 to columns once it holds ≥3 windows:

```lua
function on_window_open(win)
    if win:app_id() == "mpv" then win:set_floating(true) end
    if win:tags() == rwl.tag(2) and rwl.count(rwl.tag(2)) >= 3 then
        rwl.set_layout("col")
    end
end

function on_window_close(win)
    if rwl.count(rwl.tag(2)) < 3 then rwl.set_layout("tile") end
end

function on_tag_switch(old, new)
    rwl.notify("tag " .. new)
end
```

**Read helpers** query live state at call time:

- `rwl.count(mask)` — how many windows on the selected monitor carry any tag in
  `mask`.
- `rwl.tag(n)` — the bitmask for tag `n`.
- `win:tags()` — a window's tag bitmask.
- `win:app_id()` — a window's app-id.

**Action helpers** (`rwl.set_layout(name_or_index)`, `rwl.notify(text)`,
`win:set_floating(bool)`, …) do **not** mutate compositor state synchronously —
they enqueue a command that is applied at a safe point (right after the callback
returns, and once per event-loop iteration). This avoids re-entering the
`&mut Rwl` borrow that is running the event. See
[`src/features/hooks.rs`](../src/features/hooks.rs) for the full API and the
`HookCmd` command set.

---

## Reloading

`reload_config` (bound to `M`+`r` by default, or `rwl msg reloadconfig`) re-reads
`config.lua` at runtime and swaps in the new `Config`. On a parse error the old
running config is *not* replaced by defaults in-place in the same way as
startup; the error is logged and surfaced through the config-error channel (used
e.g. by the bar to display a warning).
