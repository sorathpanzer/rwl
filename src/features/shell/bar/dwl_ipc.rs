#![allow(clippy::wildcard_imports, clippy::single_component_path_imports, unused_imports)]
use wayland_client;
use wayland_client::protocol::*;

pub mod __interfaces {
    use wayland_client::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("protocols/dwl-ipc-unstable-v2.xml");
}
use self::__interfaces::*;

wayland_scanner::generate_client_code!("protocols/dwl-ipc-unstable-v2.xml");
