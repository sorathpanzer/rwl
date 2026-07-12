//! `wl_seat` handler — focus, cursor, data device.

use smithay::input::dnd::{DnDGrab, DndGrabHandler, DndTarget, GrabType};
use smithay::input::keyboard::LedState;
use smithay::input::pointer::{CursorImageStatus, Focus};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Point};
use smithay::wayland::selection::data_device::{
    set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::input::dnd::Source;
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::wlr_data_control::{DataControlHandler, DataControlState};

use smithay::wayland::tablet_manager::TabletSeatHandler;

use crate::state::Rwl;

impl SeatHandler for Rwl {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = self.display_handle.clone();
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(&dh, seat, client.clone());
        set_primary_focus(&dh, seat, client);
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        self.update_keyboard_leds(led_state);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // Track the last visible surface cursor for use as a software fallback
        // when the pointer is on empty space (Named / Hidden status, TTY backend).
        //
        // Critically, only update last_cursor_surface on a Named → Surface
        // transition (the client's *first* cursor after gaining pointer focus),
        // NOT on Surface → Surface.  Video players hide the cursor by switching
        // to a transparent/null surface while still holding pointer focus; if we
        // updated last_cursor_surface on every Surface event, the hiding cursor
        // would overwrite the previously-visible arrow and the fallback would
        // render an invisible cursor on empty tags.
        if let CursorImageStatus::Surface(ref surf) = image
            && surf.is_alive()
            && matches!(self.cursor_status, CursorImageStatus::Named(_))
        {
            self.last_cursor_surface = Some(surf.clone());
        }
        self.cursor_status = image;
    }
}

impl SelectionHandler for Rwl {
    type SelectionUserData = ();
}

impl DataControlHandler for Rwl {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

impl WaylandDndGrabHandler for Rwl {
    fn dnd_requested<S: Source + 'static>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: smithay::utils::Serial,
        type_: GrabType,
    ) {
        // Store the drag icon surface so the renderer can draw it at the cursor.
        self.dnd_icon = icon;

        match type_ {
            GrabType::Pointer => {
                let Some(pointer) = seat.get_pointer() else { return };
                let Some(start_data) = pointer.grab_start_data() else { return };
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start_data, source, seat),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let Some(touch) = seat.get_touch() else { return };
                let Some(start_data) = touch.grab_start_data() else { return };
                touch.set_grab(
                    self,
                    DnDGrab::new_touch(&self.display_handle, start_data, source, seat),
                    serial,
                );
            }
        }
    }
}

impl DndGrabHandler for Rwl {
    fn dropped(
        &mut self,
        _target: Option<DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.dnd_icon = None;
    }

    fn cancelled(&mut self, _seat: Seat<Self>, _location: Point<f64, Logical>) {
        self.dnd_icon = None;
    }
}

impl TabletSeatHandler for Rwl {}

impl DataDeviceHandler for Rwl {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl PrimarySelectionHandler for Rwl {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}
