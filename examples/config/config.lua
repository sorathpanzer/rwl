-- rwl compositor configuration
-- Loaded at startup from ~/.config/rwl/config.lua.
-- All fields are optional; missing keys fall back to the compiled-in defaults.
--
-- MODIFIER STRINGS  (concatenate, order doesn't matter)
--   M = Super/Logo    C = Ctrl    S = Shift    A = Alt    "" = no mods
--
-- KEY NAMES  (XKB keysym names)
--   Single printable ASCII characters: use the character itself ("a", "1", "/")
--   Named keys: "Return", "BackSpace", "Delete", "Escape", "Tab",
--               "Up", "Down", "Left", "Right", "Home", "End",
--               "Page_Up", "Page_Down", "Insert", "Print", "Pause",
--               "F1"…"F12",
--               "space", "exclam", "quotedbl", "numbersign", "dollar",
--               "percent", "ampersand", "asterisk", "underscore",
--               "colon", "semicolon", "at", "bracketleft", "bracketright",
--               "asciicircum", "grave", "braceleft", "bar", "braceright",
--               "asciitilde", "Terminate_Server",
--               "XF86Switch_VT_1" … "XF86Switch_VT_12",
--               "XF86AudioRaiseVolume", "XF86AudioLowerVolume", "XF86AudioMute",
--               "XF86AudioPlay", "XF86AudioStop", "XF86AudioNext", "XF86AudioPrev",
--               "XF86MonBrightnessUp", "XF86MonBrightnessDown"

-- ─── Appearance ───────────────────────────────────────────────────────────────
local camo = 0x658a2aff
local tragedy = 0xdf73ffff
local heaven = 0x33ccffee
local MYTHEME = tragedy

-- bar_cmd = {"waybar"}
bar = dofile(os.getenv("HOME") .. "/.config/rwl/bar.lua")

-- Keybindings and mouse buttons live in their own file.
local binds = dofile(os.getenv("HOME") .. "/.config/rwl/keys.lua")
keys    = binds.keys
buttons = binds.buttons

-- ─── Tags ────────────────────────────────────────────────────────────────────
-- number = how many tags exist (drives masks, monitor state, wallpapers).
-- names  = per-tag bar labels (one string per tag); read by the bar.

tags = {
    names  = { " ", " ", " ", " ", " ", " " },
    number = 6,
}

windows = {
    follow    = true,   -- follow windows to their new tag
    border_px = 1,
    gaps_px   = 10,
    smart_gaps = false,   -- gap even a single window (default true = smartgaps on)
    corner_radius = 15,
    auto_back_empty_tag = true,
    
    -- Colors: 0xRRGGBBAA hex integer  OR  {r, g, b, a} float table
    border_color  = 0x99999955,
    focus_color   = MYTHEME,
    fullscreen_bg = 0x00000000,
}

wallpaper = {
    -- tags = {
    --     [1] = "~/Imagens/Wallpapers/cellowolf.png",
    --     [3] = "~/Imagens/Wallpapers/minimal-sun.png",
    --     [4] = "~/Imagens/Wallpapers/misty.jpg",
    -- },
    mode = "fill",
    default = "~/Imagens/Wallpapers/candle2.jpg",
}

effects = {
    fade_out_ms = 100,
    fade_in_ms = 0,
    tag_transition    = true,   -- enable/disable (default: true)
    tag_transition_ms = 100,    -- duration in ms (default: 250)
    startup_zoom_ms   = 2000,   -- 0 disables it
    startup_zoom_amount = 0.2,  -- <1.0 zooms in; edges show fullscreen_bg until it settles
    startup_zoom_direction = "in",   -- "in" = grow into place, "out" = shrink into place
}

-- ─── Window rules ─────────────────────────────────────────────────────────────
-- Fields: id, title, tags, switch_to_tag, floating, monitor, scratch_key
-- id / title: substring match;  omit to match all

rules = {
    { id = "chromium-browser", tags = 1,        switch_to_tag = true  },
    { id = "firefox",          tags = 1,        switch_to_tag = true  },
    { id = "foot",             tags = 1 << 1,   switch_to_tag = true  },
    { id = "imv",              tags = 1 << 2,   switch_to_tag = true, no_swallow = true  },
    { id = "mpv",              tags = 1 << 3,   switch_to_tag = true  },
    { title = "qutebrowser",   tags = 1 << 4,   switch_to_tag = true  },
    { title = "Kdenlive",      tags = 1 << 4,   switch_to_tag = true  },
    { title = "LibreOffice",   tags = 1 << 4,   switch_to_tag = true  },
    { id = "dev.zed.Zed",      tags = 1 << 4,   switch_to_tag = true  },
    { id = "qemu",             tags = 1 << 4,   switch_to_tag = true  },
    { id = "obsidian",         tags = 1 << 4,   switch_to_tag = true  },
    { id = "blender",          tags = 1 << 4,   switch_to_tag = true  },
    { title = "Signal",        tags = 1 << 5,   switch_to_tag = true  },
    { id = "electron",         tags = 1 << 5,   switch_to_tag = true  },
    { title = "HyprWall",      floating = true                        },
    { id = "scratchpad",       floating = true, scratch_key = "s"     },
}

-- swallow = { enabled = false, terminals = { "foot", "kitty" } }

-- ─── Layouts ─────────────────────────────────────────────────────────────────
-- kind: "tile" | "monocle" | "floating" | "col"

layouts = {
    { symbol = "[]=", kind = "tile"     },
    { symbol = "[M]", kind = "monocle"  },
    { symbol = "||",  kind = "col"      },
    { symbol = ">>>", kind = "scroll"   },
    { symbol = "[@]", kind = "dwindle"  },
    { symbol = "(B)", kind = "bstack"  },
    { symbol = "|-|", kind = "centeredmaster"  },
    { symbol = "[G]", kind = "lua" },   -- your on_arrange
}

function on_arrange(n, area, kind)
  if kind == "lua" then return grid(n, area) end
  local cols = math.ceil(math.sqrt(n))
  local rows = math.ceil(n / cols)
  local cw, ch = math.floor(area.w / cols), math.floor(area.h / rows)
  local out = {}
  for i = 1, n do
    local c = (i - 1) % cols
    local r = math.floor((i - 1) / cols)
    out[i] = { x = area.x + c * cw, y = area.y + r * ch, w = cw, h = ch }
  end
  return out
end

-- ─── Monitor rules ────────────────────────────────────────────────────────────
-- name: prefix match; omit for catch-all.
-- layout: 1-based index into layouts above (1 = tile).
-- transform: "normal"|"90"|"180"|"270"|"flipped"|"flipped90"|"flipped180"|"flipped270"

monitor_rules = {
    mfact = 0.55,
    nmaster = 1,
    scale = 1.0,
    layout = 0,
    transform = "normal", x = -1, y = -1,
    vrr = "off", -- on, off, ondemand
    -- name = "eDP-1",
}

-- ─── Keyboard ────────────────────────────────────────────────────────────────

keyboard = {
    xkb_layout = "pt",
    numlock    = true,
    capslock   = false,
    repeat_rate  = 25,
    repeat_delay = 600,
}

-- ─── Pointer / libinput ──────────────────────────────────────────────────────
-- scroll_method:  "no_scroll" | "two_finger" | "edge" | "on_button_down"
-- click_method:   "none" | "button_areas" | "clickfinger"
-- accel_profile:  "none" | "flat" | "adaptive"
-- tap_button_map: "left_middle_right" | "left_right_middle"

mouse = {
    tap_to_click            = true,
    tap_and_drag            = true,
    drag_lock               = true,
    natural_scrolling       = true,
    disable_while_typing    = true,
    left_handed             = false,
    middle_button_emulation = false,
    scroll_method           = "two_finger",
    click_method            = "button_areas",
    accel_profile           = "adaptive",
    accel_speed             = 0.0,
    tap_button_map          = "left_right_middle",
    mouse_focus              = true,   -- focus follows mouse
    cursor_timeout          = 5,   -- seconds before cursor hides
    warp_cursor = true,
    cursor_theme = "Adwaita",
    cursor_size = 24,
}

overview = {
    hint_keys       = "123456789",  -- numbers used for jump hints
    dim             = 0x00000099,   -- backdrop color 0xRRGGBBAA (~60% black)
    padding         = 100,           -- gap around/between thumbnails (logical px)
    anim_ms         = 10,          -- zoom animation duration
    highlight_color = tragedy,   -- ring around hovered/matched thumbnail
    highlight_px    = 2,
    border_color    = 0x888888aa,   -- NEW: ring on every other thumbnail
    border_px       = 1,
}

pip = {
    width        = 480,             -- thumbnail width (px); height follows aspect
    margin       = 24,              -- gap from screen edges
    border_px    = 2,
    border_color = heaven,
    corner       = "top-right",  -- top-left | top-right | bottom-left | bottom-right
}

-- ─── Auto-spawn per tag ───────────────────────────────────────────────────────
-- Index = tag number (1-based).  nil = no auto-spawn for that tag.

auto_spawn = {
    [1] = "firefox",  -- tag 1
    [2] = "foot",      -- tag 2
}

startup_cmds = {
    {"gammastep", "-O", "4000"},
    {"udiskie"},
}

pertag_layouts = {
  [1] = "monocle",
  [2] = { "monocle", "tile", "tile", "bstack", "centeredmaster" },
  [3] = "col",
  [4] = "col",
  [6] = "scroll",
}
