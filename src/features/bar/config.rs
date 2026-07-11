use azoth_render::RustColor;

pub const DEFAULT_FONT: &str = "UbuntuMono Nerd Font:style=Bold:size=14";
pub const DEFAULT_VERTICAL_PADDING: u32 = 4;
pub const DEFAULT_BUFFER_SCALE: u32 = 1;
pub const DEFAULT_TAG_NAMES: &[&str] = &["1","2","3","4","5","6"];

#[derive(Clone)]
pub struct Block {
    pub icon:     String,
    pub command:  String,
    pub interval: u32,
    /// SIGRTMIN+signal triggers an immediate refresh; 0 = signal-driven updates disabled.
    pub signal:   u8,
}

#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub ipc: bool,
    pub hidden: bool,
    pub bottom: bool,
    pub hide_vacant: bool,
    pub status_commands: bool,
    pub center_title: bool,
    pub active_color_title: bool,
    pub font_str: String,
    pub vertical_padding: u32,
    pub buffer_scale: u32,

    pub active_fg_color:          RustColor,
    pub active_bg_color:          RustColor,
    pub occupied_fg_color:        RustColor,
    pub occupied_bg_color:        RustColor,
    pub inactive_fg_color:        RustColor,
    pub inactive_bg_color:        RustColor,
    pub urgent_fg_color:          RustColor,
    pub urgent_bg_color:          RustColor,
    pub middle_bg_color:          RustColor,
    pub middle_bg_color_selected: RustColor,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ipc: true,
            hidden: false,
            bottom: false,
            hide_vacant: true,
            status_commands: true,
            center_title: true,
            active_color_title: true,
            font_str: DEFAULT_FONT.to_owned(),
            vertical_padding: DEFAULT_VERTICAL_PADDING,
            buffer_scale: DEFAULT_BUFFER_SCALE,

            active_fg_color:          RustColor::opaque(0xdf, 0x73, 0xff),
            active_bg_color:          RustColor::opaque(0x00, 0x00, 0x00),
            occupied_fg_color:        RustColor::opaque(0x99, 0x99, 0x99),
            occupied_bg_color:        RustColor::opaque(0x00, 0x00, 0x00),
            inactive_fg_color:        RustColor::opaque(0x99, 0x99, 0x99),
            inactive_bg_color:        RustColor::opaque(0x00, 0x00, 0x00),
            urgent_fg_color:          RustColor::opaque(0x00, 0x00, 0x00),
            urgent_bg_color:          RustColor::opaque(0xee, 0xee, 0xee),
            middle_bg_color:          RustColor::opaque(0x00, 0x00, 0x00),
            middle_bg_color_selected: RustColor::opaque(0x00, 0x00, 0x00),
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
const fn hex_to_rust_color(hex: u32) -> RustColor {
    RustColor {
        red:   ((hex >> 24) as u8 as u16) * 0x101,
        green: (((hex >> 16) & 0xff) as u8 as u16) * 0x101,
        blue:  (((hex >>  8) & 0xff) as u8 as u16) * 0x101,
        alpha: ((hex & 0xff) as u8 as u16) * 0x101,
    }
}

impl Config {
    pub fn from_settings(s: &super::BarSettings) -> Self {
        Self {
            ipc:                true,
            hidden:             s.hidden,
            bottom:             s.bottom,
            hide_vacant:        s.hide_vacant,
            status_commands:    s.status_commands,
            center_title:       s.center_title,
            active_color_title: s.active_color_title,
            font_str:           s.font.clone(),
            vertical_padding:   s.vertical_padding,
            buffer_scale:       s.buffer_scale,
            active_fg_color:          hex_to_rust_color(s.active_fg),
            active_bg_color:          hex_to_rust_color(s.active_bg),
            occupied_fg_color:        hex_to_rust_color(s.occupied_fg),
            occupied_bg_color:        hex_to_rust_color(s.occupied_bg),
            inactive_fg_color:        hex_to_rust_color(s.inactive_fg),
            inactive_bg_color:        hex_to_rust_color(s.inactive_bg),
            urgent_fg_color:          hex_to_rust_color(s.urgent_fg),
            urgent_bg_color:          hex_to_rust_color(s.urgent_bg),
            middle_bg_color:          hex_to_rust_color(s.middle_bg),
            middle_bg_color_selected: hex_to_rust_color(s.middle_bg_selected),
        }
    }
}

pub fn blocks_from_settings(s: &super::BarSettings) -> Vec<Block> {
    s.blocks.iter().map(|b| Block {
        icon:     b.icon.clone(),
        command:  b.command.clone(),
        interval: b.interval,
        signal:   b.signal,
    }).collect()
}
