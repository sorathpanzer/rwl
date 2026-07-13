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

Grouped tables: `windows`, `wallpaper`, `effects`, `keyboard`, `mouse`, `bar`,
`overview`, `pip`, plus `rwl` (the hooks namespace). Each is described below.

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

### `wallpaper`

Requires the `wallpaper` feature.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `tags` | table | none | Per-tag wallpapers: `{ [tag] = "path", … }`. Preloaded at startup. |
| `default` | string | none | Fallback wallpaper image path (`~` expanded; PNG/JPEG), used for tags with no `tags` entry. |
| `mode` | string | `fill` | Fit mode: `fill`, `fit`, `stretch`, `center`. |

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

The special kind **`lua`** (`hooks` feature) runs your own tiling algorithm — the
global `on_arrange` callback (see [Lua hooks](#lua-hooks)). Add it like any other
layout and give it whatever `symbol` you like:

```lua
layouts = {
  { symbol = "[]=", kind = "tile" },
  { symbol = "[L]", kind = "lua" },
}
```

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
function on_tag_switch(old, new) end -- also fired once at startup (old == 0)
function on_focus(win) end
function on_title_change(win, title) end
function on_fullscreen(win, fs) end   -- fs is a bool; user/client-initiated only
function on_layout_change(old, new) end -- old/new are layout symbol names
function on_monitor_add(name) end     -- output plugged in (also the first one)
function on_monitor_remove(name) end  -- output unplugged
function on_startup() end             -- fired once, after the first output
```

`on_fullscreen` fires only for user- or client-initiated fullscreen changes, not
for `win:set_fullscreen()` calls made from Lua (to avoid feedback loops).
`on_layout_change` also fires when a per-tag layout changes as a side effect of
switching tags. `on_monitor_add` fires after `on_startup` for the first output,
and on every hotplug thereafter.

`on_tag_switch` is also fired once at startup with `old == 0` and `new` set to
the launch tag, so per-tag state (such as a wallpaper) is applied immediately
instead of only after the first manual tag switch. Use `on_startup` for one-time
setup that isn't tag-specific.

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

Example — a different wallpaper per tag (`wallpaper` feature):

```lua
local papers = {
    [rwl.tag(1)] = "~/pics/forest.png",
    [rwl.tag(2)] = "~/pics/city.jpg",
}

function on_tag_switch(old, new)
    if papers[new] then rwl.set_wallpaper(papers[new], "fill") end
end

-- Decode every wallpaper up front (off the render thread) so the first switch
-- to each tag has no decode lag. on_tag_switch(0, launch_tag) fires right after
-- this, applying the launch tag's wallpaper.
function on_startup()
    for _, path in pairs(papers) do rwl.preload(path) end
end
```

`rwl.preload(path)` decodes on a background thread and caches the result; it
does not change what is on screen. Cached images (whether preloaded or shown
once) are reused, so a tag revisit never re-decodes.

**Read helpers** query live state at call time (answered from a snapshot taken
right before the callback fires):

- `rwl.count(mask)` — how many windows on the selected monitor carry any tag in
  `mask`.
- `rwl.tag(n)` — the bitmask for tag `n`.
- `rwl.clients()` — array of window handles visible on the current tagset.
- `rwl.focused()` — the focused window handle, or `nil`.
- `rwl.sel_tags()` — the selected monitor's active tag bitmask.
- `rwl.layout()` — the selected monitor's layout symbol name (or `nil`).
- `rwl.monitors()` — number of connected monitors.
- `win:app_id()`, `win:title()`, `win:tags()`, `win:is_floating()`,
  `win:is_fullscreen()`, `win:monitor()` — per-window state.

**Action helpers** do **not** mutate compositor state synchronously —
they enqueue a command that is applied at a safe point (right after the callback
returns, and once per event-loop iteration). This avoids re-entering the
`&mut Rwl` borrow that is running the event.

- `rwl` table: `spawn`, `view`, `toggle_view(mask)`, `set_layout(name_or_index)`,
  `zoom()`, `inc_nmaster(delta)`, `set_mfact(delta)`, `focus_monitor(dir)`,
  `notify(text)`, `set_wallpaper(path[, mode])` / `preload(path)` (`wallpaper`
  feature; a nil path clears it).
- `win` handle: `set_tags(mask)`, `toggle_tag(mask)`, `set_floating(bool)`,
  `set_fullscreen(bool)`, `focus()`, `close()`, `move_to_monitor(dir)`,
  `warp()` (`warp` feature), `set_scratch(key)` (`scratchpad` feature).

The selected-monitor / focused-window helpers (`zoom`, `inc_nmaster`,
`set_mfact`, `focus_monitor`, `toggle_view`) act exactly like the matching
keybinding. See
[`src/features/hooks.rs`](../src/features/hooks.rs) for the full API and the
`HookCmd` command set.

### Scriptable layouts (`on_arrange`)

Define a `lua` layout (see [Layouts](#layouts)) and rwl calls your global
`on_arrange(n, area, symbol)` to place the `n` tiled windows:

- **`n`** — the number of tiled windows (always ≥ 1 when called).
- **`area`** — the work-area rectangle plus the monitor's master settings:
  `{ x, y, w, h, mfact, nmaster }`. `mfact` (0.05–0.95) and `nmaster` follow the
  usual `set_mfact` / `inc_nmaster` keybindings, so honouring them makes your
  layout respond to them for free.
- **`symbol`** — the active layout's symbol string (see *Multiple layouts* below).

It must **return an array of `n` `{ x, y, w, h }` rectangles** in tiling order.
Values are read as numbers and rounded. Anything invalid — a Lua error, the wrong
element count, or no `on_arrange` at all — safely falls back to filling the work
area, so a window is never lost.

Things to know:

- **Outer cells.** Tile the work area edge-to-edge; rwl reserves the border
  *inside* each cell for you, so borders never spill off screen (a lone window
  gets no border, like the built-in layouts). Do **not** subtract `border_px`
  yourself. Want extra spacing? Inset each rect by some `g` of your own.
- **Absolute coordinates.** `area` already excludes bars and exclusive
  layer-shell zones and carries the monitor's `area.x`/`area.y`, so anchor rects
  at `area.x`/`area.y` and a layout works unchanged on a second monitor.
- **Hot path.** `on_arrange` runs on every re-arrange — keep it cheap and
  side-effect-free (compute geometry; don't `spawn`/`view`). Floating and
  fullscreen windows are handled by the compositor and never passed to it.
- **Live editing.** Edit `on_arrange` and run `reload_config` (`M`+`r`) to apply
  it without restarting.

**Multiple layouts** all share the one `on_arrange`; branch on `symbol` to tell
them apart (each `layouts` entry sets its own symbol):

```lua
layouts = {
  { symbol = "[]=", kind = "tile" },   -- built-in
  { symbol = "|||", kind = "lua" },    -- columns  (default below)
  { symbol = "[M]", kind = "lua" },    -- master + stack
  { symbol = "[G]", kind = "lua" },    -- grid
  { symbol = "[@]", kind = "lua" },    -- spiral
}

function on_arrange(n, area, symbol)
  if     symbol == "[M]" then return master_stack(n, area)
  elseif symbol == "[G]" then return grid(n, area)
  elseif symbol == "[@]" then return spiral(n, area)
  else                        return columns(n, area) end
end
```

#### Example layouts

Each of these is a plain function you can drop in and wire up through the
`on_arrange` dispatcher above.

```lua
-- Columns: n equal full-height columns, left to right.
function columns(n, area)
  local out, w = {}, math.floor(area.w / n)
  for i = 1, n do
    out[i] = { x = area.x + (i - 1) * w, y = area.y, w = w, h = area.h }
  end
  return out
end

-- Rows: n equal full-width rows, top to bottom.
function rows(n, area)
  local out, h = {}, math.floor(area.h / n)
  for i = 1, n do
    out[i] = { x = area.x, y = area.y + (i - 1) * h, w = area.w, h = h }
  end
  return out
end

-- Master + stack: the dwm classic. First `nmaster` windows share a master
-- column of width `mfact`; the rest stack in the remaining column. Honours the
-- live mfact / nmaster keybindings.
function master_stack(n, area)
  local out = {}
  local nm = math.min(area.nmaster, n)
  if nm == n then return columns_vertical(n, area) end       -- no stack yet
  local mw = math.floor(area.w * area.mfact)
  local sw, mh = area.w - mw, math.floor(area.h / nm)
  local sh = math.floor(area.h / (n - nm))
  for i = 1, n do
    if i <= nm then
      out[i] = { x = area.x, y = area.y + (i - 1) * mh, w = mw, h = mh }
    else
      local k = i - nm - 1
      out[i] = { x = area.x + mw, y = area.y + k * sh, w = sw, h = sh }
    end
  end
  return out
end

-- helper: stack all n windows in one full-width column (used above when
-- everything is master).
function columns_vertical(n, area)
  local out, h = {}, math.floor(area.h / n)
  for i = 1, n do
    out[i] = { x = area.x, y = area.y + (i - 1) * h, w = area.w, h = h }
  end
  return out
end

-- Bottom-stack: master row on top (height `mfact`), stack row of columns below.
function bstack(n, area)
  local out = {}
  local nm = math.min(area.nmaster, n)
  if nm == n then return columns(n, area) end
  local mh = math.floor(area.h * area.mfact)
  local sh, mw = area.h - mh, math.floor(area.w / nm)
  local sw = math.floor(area.w / (n - nm))
  for i = 1, n do
    if i <= nm then
      out[i] = { x = area.x + (i - 1) * mw, y = area.y, w = mw, h = mh }
    else
      local k = i - nm - 1
      out[i] = { x = area.x + k * sw, y = area.y + mh, w = sw, h = sh }
    end
  end
  return out
end

-- Grid: near-square arrangement (row-major).
function grid(n, area)
  local cols = math.ceil(math.sqrt(n))
  local rws  = math.ceil(n / cols)
  local cw, ch = math.floor(area.w / cols), math.floor(area.h / rws)
  local out = {}
  for i = 1, n do
    local c, r = (i - 1) % cols, math.floor((i - 1) / cols)
    out[i] = { x = area.x + c * cw, y = area.y + r * ch, w = cw, h = ch }
  end
  return out
end

-- Spiral / fibonacci: each window takes half of the shrinking remainder,
-- alternating vertical and horizontal splits.
function spiral(n, area)
  local out = {}
  local x, y, w, h = area.x, area.y, area.w, area.h
  for i = 1, n do
    if i == n then
      out[i] = { x = x, y = y, w = w, h = h }
    elseif i % 2 == 1 then                 -- take the left half
      local half = math.floor(w / 2)
      out[i] = { x = x, y = y, w = half, h = h }
      x, w = x + half, w - half
    else                                   -- take the top half
      local half = math.floor(h / 2)
      out[i] = { x = x, y = y, w = w, h = half }
      y, h = y + half, h - half
    end
  end
  return out
end

-- Deck: one main window on top (`mfact` of the height), the rest as a strip of
-- equal thumbnails along the bottom.
function deck(n, area)
  if n == 1 then
    return { { x = area.x, y = area.y, w = area.w, h = area.h } }
  end
  local mh = math.floor(area.h * area.mfact)
  local out = { { x = area.x, y = area.y, w = area.w, h = mh } }
  local tw, th, ty = math.floor(area.w / (n - 1)), area.h - mh, area.y + mh
  for i = 2, n do
    out[i] = { x = area.x + (i - 2) * tw, y = ty, w = tw, h = th }
  end
  return out
end

-- Centered reading column: a single centred column of fixed max width, windows
-- stacked; good for wide monitors.
function centered(n, area)
  local out = {}
  local w = math.min(area.w, 1200)
  local x = area.x + math.floor((area.w - w) / 2)
  local h = math.floor(area.h / n)
  for i = 1, n do
    out[i] = { x = x, y = area.y + (i - 1) * h, w = w, h = h }
  end
  return out
end
```

---

## Reloading

`reload_config` (bound to `M`+`r` by default, or `rwl msg reloadconfig`) re-reads
`config.lua` at runtime and swaps in the new `Config`. On a parse error the old
running config is *not* replaced by defaults in-place in the same way as
startup; the error is logged and surfaced through the config-error channel (used
e.g. by the bar to display a warning).
