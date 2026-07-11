//! `wlr-layer-shell` handler — manages bars, overlays, wallpapers, etc.

use smithay::desktop::{layer_map_for_output, LayerSurface as DesktopLayerSurface};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};

use crate::state::Rwl;

impl WlrLayerShellHandler for Rwl {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        // Wrap the wlr surface in the desktop LayerSurface type.
        let desktop_surface = DesktopLayerSurface::new(surface, namespace);

        // Map to the specified output, or fall back to selected monitor.
        let output: Option<Output> = wl_output
            .as_ref()
            .and_then(|o| {
                self.monitors
                    .iter()
                    .find(|m| m.output.owns(o))
                    .map(|m| m.output.clone())
            })
            .or_else(|| self.sel_monitor().map(|m| m.output.clone()));

        if let Some(output) = output {
            let mut map = layer_map_for_output(&output);
            map.map_layer(&desktop_surface).ok();
            // Do NOT call arrange() here: set_anchor/set_size/commit from the
            // client haven't been dispatched yet, so the pending state is all
            // defaults.  arrange() would send a configure(0,0) which causes
            // the client to allocate a zero-byte shm pool and crash.
            // The commit() handler calls arrange_all() on the first client
            // commit, at which point the committed state is correct.
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        // Unmap from whichever output had this layer surface.
        if let Some(mon) = self.monitors.iter().find(|mon| {
            layer_map_for_output(&mon.output).layers().any(|l| l.layer_surface() == &surface)
        }) {
            let mut map = layer_map_for_output(&mon.output);
            let dl = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(dl) = dl {
                map.unmap_layer(&dl);
            }
        }
        self.arrange_all();
        // Restore focus: either to another interactive layer or back to windows.
        self.update_layer_focus();
    }
}

