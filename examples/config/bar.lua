-- Bar configuration, split out of config.lua.
-- Loaded via:  bar = dofile(os.getenv("HOME") .. "/.config/rwl/bar.lua")
--
-- This file must `return` the bar table DIRECTLY (not wrapped in { bar = ... }),
-- because config.lua assigns its result straight to the global `bar`.
--
-- NOTE: locals from config.lua (MYTHEME, camo, tragedy, ...) are NOT visible
-- here — each dofile chunk has its own scope. Redefine any colors you need.

local tragedy = 0xdf73ffff
local MYTHEME = tragedy

-- Status blocks: icon + shell command + update interval (secs) + signal
--   interval = 0 : never update on timer (only on signal)
--   signal   = N : SIGRTMIN+N triggers an immediate refresh

return {
    font             = "UbuntuMono Nerd Font:style=Bold:size=14",
    vertical_padding = 4,
    buffer_scale     = 1,
    hidden           = false,
    bottom           = false,
    hide_vacant      = true,
    center_title     = true,
    active_color_title = true,
    status_commands  = true,

    -- Colors: 0xRRGGBBAA
    active_fg          = MYTHEME,
    active_bg          = 0x000000ff,
    occupied_fg        = 0x999999ff,
    occupied_bg        = 0x000000ff,
    inactive_fg        = 0x999999ff,
    inactive_bg        = 0x000000ff,
    urgent_fg          = 0x000000ff,
    urgent_bg          = 0xeeeeeeff,
    middle_bg          = 0x000000ff,
    middle_bg_selected = 0x000000ff,

    blocks_delim = "  ",

    -- Tag labels now live in config.lua's `tags.names` (alongside `tags.number`).


    blocks = {
        { icon = "🗄️", command = "df -h | rg /dev/dm-0 | awk '{print $5}'",          interval = 3600, signal = 0 },
        { icon = "💻", command = "free -h | awk '/^Mem/ { print $3 }' | sed s/i//g", interval = 30,   signal = 0 },
        -- { icon = "", command = "lister",                                           interval = 0,    signal = 1 },
        {
          icon = "",
          command = [[
            muted=$(pactl get-sink-mute @DEFAULT_SINK@ | awk '{print $2}')
            vol=$(pactl get-sink-volume @DEFAULT_SINK@ | grep -oP '\d+(?=%)' | head -1)
            if [ "$muted" = "sim" ]; then printf "🔇%s%%" "$vol"
            elif [ "$vol" -eq 0 ];   then printf "🔈%s%%" "--"
            elif [ "$vol" -lt 33 ];  then printf "🔉%s%%" "$vol"
            elif [ "$vol" -lt 66 ];  then printf "🔊%s%%" "$vol"
            else                          printf "🔊%s%%" "$vol"
            fi
          ]], interval = 0, signal = 1
        },
        { icon = "🕓", command = "nu -c 'date now | format date %H:%M'", interval = 1, signal = 0,
           command_click = "rwl msg lock", hover_command = "cal --color=always" },
    },
}
