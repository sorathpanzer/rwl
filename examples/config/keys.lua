-- Keybindings and mouse buttons, split out of config.lua.
-- Loaded via:  local binds = dofile(os.getenv("HOME") .. "/.config/rwl/keys.lua")
--              keys, buttons = binds.keys, binds.buttons
--
-- This file is self-contained: it defines its own M/C/S/A modifier locals and
-- helpers, because dofile chunks do NOT see config.lua's locals. It returns a
-- table { keys = ..., buttons = ... }.
--
-- Actions and their extra fields:
--   spawn                    cmd = {"prog", "arg1", ...}
--   focus_stack              dir = 1 or -1
--   inc_nmaster              delta = 1 or -1
--   set_mfact                delta = 0.05 or -0.05
--   zoom / view_prev / kill_client / toggle_floating / toggle_fullscreen
--   toggle_passthrough / move / resize / quit   (no extra fields)
--   set_layout               index = 0..N-1  (usize::MAX = toggle)
--   view / toggle_view / tag / toggle_tag / view_tag_spawn   mask = bitmask
--   view_next_occ_tag        dir = 1 or -1
--   focus_mon / tag_mon      dir = -1 or 1
--   toggle_scratch / focus_or_toggle_scratch / focus_or_toggle_matching_scratch
--                            scratch_key = "s",  cmd = {"foot", ...}
--   chvt                     vt = 1..12

local M = "M"
local C = "C"
local S = "S"
local A = "A"

local tag_count = 6
local TAG_MASK  = (1 << tag_count) - 1

local scratch_s = {
    scratch_key = "s",
    cmd = {"foot", "-a", "scratchpad",
           "--override=colors-dark.alpha=1",
           "--override=colors-dark.background=000000"},
}

-- helper: build a scratch binding table
local function scratch(mods, key, action)
    return {
        mods = mods, key = key, action = action,
        scratch_key = scratch_s.scratch_key,
        cmd         = scratch_s.cmd,
    }
end

-- Helper for running shell commands as a single string
local function sh(cmd_str)
    return {"/bin/sh", "-c", cmd_str}
end

local keys = {
    -- Terminal / launcher
    -- { mods=M..S, key="Return", action="spawn", cmd={"wmenu-run","-f","12","-N","000000","-S","74438e"} },
    { mods=M,    key = "s", action = "reset_layout" },
    { mods=M,    key="p", action="toggle_pip" },
    { mods=M..S, key="P", action="move_pip"   },
    { mods=M,    key="", action="toggle_overview_all"     },
    { mods=M..S, key="", action="toggle_overview" },
    { mods=M..S, key="Return", action="bar_prompt"          },
    { mods=M,    key="Return", action="spawn", cmd={"foot"} },
    { mods=M,    key="b",      action="toggle_bar"          },
    -- Focus navigation
    { mods=M,    key="Down",   action="focus_stack", dir=1  },
    { mods=M,    key="Up",     action="focus_stack", dir=-1 },
    { mods=M,    key="Right",  action="focus_stack", dir=1  },
    { mods=M,    key="Left",   action="focus_stack", dir=-1 },
    -- Master count / size
    { mods=M,     key="i",     action="inc_nmaster", delta=1     },
    { mods=M,     key="u",     action="inc_nmaster", delta=-1    },
    { mods=M..C,  key="+",     action="set_mfact",   delta=-0.05 },
    { mods=M..C,  key="-",     action="set_mfact",   delta=0.05  },
    -- Tag navigation
    { mods=M..S, key="Escape", action="view_next_occ_tag", dir=-1 },
    { mods=M,    key="Escape", action="view_next_occ_tag", dir=1  },
    -- Zoom / swap master
    { mods=M, key="z",        action="zoom"      },
    { mods=M, key="\\",       action="view_prev" },
    -- Window management
    { mods=M, key="q", action="kill_client" },
    -- Layouts (index is 0-based)
    { mods=M, key="Tab", action="cycle_layout"                  },
    { mods=M, key="t",   action="set_layout", index=0           },
    { mods=M, key="o",   action="set_layout", index=1           },
    { mods=M, key="c",   action="set_layout", index=2           },
    { mods=M, key="n",   action="set_layout", index=3           },
    { mods=M, key="d",   action="set_layout", index=4           },
    { mods=M, key="a", action="set_layout", index=0x7fffffffffffffff }, -- usize::MAX = toggle
    -- Floating / fullscreen
    { mods=M, key="space", action="toggle_floating"   },
    { mods="", key="F11",     action="toggle_fullscreen" },
    -- All-tags view
    { mods=M,    key="0", action="view", mask=TAG_MASK },
    { mods=M..S, key="=", action="tag",  mask=TAG_MASK },
    -- Monitor focus / send
    { mods=M,     key=",", action="focus_mon", dir=-1 },
    { mods=M..C,  key="<", action="tag_mon",   dir=-1 },
    { mods=M..C,  key=">", action="tag_mon",   dir=1  },
    -- Scratchpad
    scratch(M, "<",   "toggle_scratch"),
    scratch(M, "h",   "focus_or_toggle_scratch"),
    scratch(M, "k",   "focus_or_toggle_matching_scratch"),
    -- Audio
    { mods=M,    key="+", action="spawn", cmd=sh("pactl set-sink-volume 0 +10% && pkill -RTMIN+1 rwl") },
    { mods=M,    key="-", action="spawn", cmd=sh("pactl set-sink-volume 0 -10% && pkill -RTMIN+1 rwl") },
    { mods=M,    key="m", action="spawn", cmd=sh("pactl set-sink-mute 0 toggle && pkill -RTMIN+1 rwl") },
    { mods=M..S, key="R", action="spawn", cmd=sh("gammastep -O 4000") },
    -- Apps
    { mods=M,    key="v", action="spawn", cmd=sh("hyprtools stream") },
    { mods=M,    key="w", action="spawn", cmd=sh("pgrep qutebrowser || qutebrowser") },
    { mods=M,    key=".", action="spawn", cmd={"searcher"} },
    -- Power
    { mods=M..S, key="BackSpace", action="spawn", cmd={"reboot"} },
    { mods=M..S, key="Delete",    action="spawn", cmd={"poweroff"} },
    { mods=M,    key="BackSpace", action="spawn", cmd=sh("systemctl suspend") },
    { mods=M,    key="Delete",    action="spawn", cmd=sh("rwl msg lock & systemctl suspend") },
    -- Backlight
    { mods=M..S, key="asterisk",   action="spawn", cmd=sh("brightnessctl set 10%+") },
    { mods=M..S, key="underscore", action="spawn", cmd=sh("brightnessctl set 10%-") },
    -- Misc
    { mods =M,  key = "g",   action = "toggle_gaps" },
    { mods="",  key="F3",    action="spawn", cmd={"hyprbg"} },
    { mods="",  key="Print", action="spawn", cmd=sh("screenshot --now") },
    { mods=M,   key="Print", action="spawn", cmd=sh("screenshot --area") },
    { mods=M,   key = "r",   action = "reload_config" },
    { mods=M..C,       key="1", action="toggle_view", mask=1<<0 },
    { mods=M..S,       key="1", action="tag",         mask=1<<0 },
    { mods=M..C..S,    key="1", action="toggle_tag",  mask=1<<0 },
    { mods=M..C,       key="2", action="toggle_view", mask=1<<1 },
    { mods=M..S,       key="2", action="tag",         mask=1<<1 },
    { mods=M..C..S,    key="2", action="toggle_tag",  mask=1<<1 },
    { mods=M,          key="3", action="view",        mask=1<<2 },
    { mods=M..C,       key="3", action="toggle_view", mask=1<<2 },
    { mods=M..S,       key="3", action="tag",         mask=1<<2 },
    { mods=M..C..S,    key="3", action="toggle_tag",  mask=1<<2 },
    { mods=M,          key="4", action="view",        mask=1<<3 },
    { mods=M..C,       key="4", action="toggle_view", mask=1<<3 },
    { mods=M..S,       key="4", action="tag",         mask=1<<3 },
    { mods=M..C..S,    key="4", action="toggle_tag",  mask=1<<3 },
    { mods=M,          key="5", action="view",        mask=1<<4 },
    { mods=M..C,       key="5", action="toggle_view", mask=1<<4 },
    { mods=M..S,       key="5", action="tag",         mask=1<<4 },
    { mods=M..C..S,    key="5", action="toggle_tag",  mask=1<<4 },
    { mods=M,          key="6", action="view",        mask=1<<5 },
    { mods=M..C,       key="6", action="toggle_view", mask=1<<5 },
    { mods=M..S,       key="6", action="tag",         mask=1<<5 },
    { mods=M..C..S,    key="6", action="toggle_tag",  mask=1<<5 },
    -- Passthrough / quit
    { mods=M,    key="x", action="toggle_passthrough" },
    { mods=M..S, key="Q", action="quit" },
    { mods=M..S, key="R", action="spawn", cmd=sh("echo '1' > ~/.cache/dwl.quit; pkill -f 'swaybg|waybar|dwl'") },
    -- Ctrl-Alt-Backspace
    { mods=C..A, key="Terminate_Server", action="quit" },
    -- VT switching (Ctrl-Alt-F1..F12)
    { mods=C..A, key="XF86Switch_VT_1",  action="chvt", vt=1  },
    { mods=C..A, key="XF86Switch_VT_2",  action="chvt", vt=2  },
    { mods=C..A, key="XF86Switch_VT_3",  action="chvt", vt=3  },
    { mods=C..A, key="XF86Switch_VT_4",  action="chvt", vt=4  },
    { mods=C..A, key="XF86Switch_VT_5",  action="chvt", vt=5  },
    { mods=C..A, key="XF86Switch_VT_6",  action="chvt", vt=6  },
    { mods=C..A, key="XF86Switch_VT_7",  action="chvt", vt=7  },
    { mods=C..A, key="XF86Switch_VT_8",  action="chvt", vt=8  },
    { mods=C..A, key="XF86Switch_VT_9",  action="chvt", vt=9  },
    { mods=C..A, key="XF86Switch_VT_10", action="chvt", vt=10 },
    { mods=C..A, key="XF86Switch_VT_11", action="chvt", vt=11 },
    { mods=C..A, key="XF86Switch_VT_12", action="chvt", vt=12 },
}

local function add_tag_keys(tbl, mods, action)
    for i = 1, tag_count do
        tbl[#tbl + 1] = {
            mods = mods,
            key = tostring(i),
            action = action,
            mask = 1 << (i - 1),
        }
    end
end

add_tag_keys(keys, M, "view_tag_spawn")

-- ─── Button bindings ─────────────────────────────────────────────────────────
-- button: "left" | "right" | "middle"

local buttons = {
    { mods=M, button="left",   action="move"             },
    { mods=M, button="middle", action="toggle_floating"  },
    { mods=M, button="right",  action="resize"           },
}

-- Hand both tables back; config.lua unpacks them into the `keys`/`buttons` globals.
return { keys = keys, buttons = buttons }
