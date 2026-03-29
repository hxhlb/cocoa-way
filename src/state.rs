use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_seat, delegate_shm,
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        selection::data_device::{DataDeviceHandler, WaylandDndGrabHandler},
        selection::SelectionHandler,
        shm::{BufferData, ShmHandler, ShmState},
    },
};
use smithay::wayland::shell::xdg::{XdgShellHandler, XdgShellState};
use smithay::wayland::shell::xdg::decoration::{XdgDecorationState, XdgDecorationHandler};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use crate::layout::Layout;
pub struct AppState {
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<AppState>,
    pub seat: Seat<Self>,
    pub data_device_state: smithay::wayland::selection::data_device::DataDeviceState,
    pub xdg_decoration_state: XdgDecorationState,
    pub output_state: smithay::wayland::output::OutputManagerState,
    pub output: smithay::output::Output,
    pub toplevels: Vec<smithay::wayland::shell::xdg::ToplevelSurface>,
    pub layout: Layout,
    pub surface_buffers: std::collections::HashMap<
        smithay::reexports::wayland_server::backend::ObjectId,
        smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    >,
    pub surface_positions: std::collections::HashMap<
        smithay::reexports::wayland_server::backend::ObjectId,
        (i32, i32),
    >,
    pub drag_state: Option<(
        smithay::reexports::wayland_server::backend::ObjectId,
        (f64, f64),
    )>,
    pub start_drag_request: Option<smithay::reexports::wayland_server::backend::ObjectId>,
    pub loop_signal: std::sync::mpsc::Sender<crate::messages::CompositorMessage>,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}
impl AppState {
    pub fn new(
        display_handle: &DisplayHandle,
        scale_factor: f64,
        loop_signal: std::sync::mpsc::Sender<crate::messages::CompositorMessage>,
        width: u32,
        height: u32,
    ) -> Self {
        let compositor_state = CompositorState::new::<Self>(display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(display_handle);
        let shm_state = ShmState::new::<Self>(
            display_handle,
            vec![
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Argb8888,
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888,
            ],
        );
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display_handle, "winit-seat");
        seat.add_keyboard(Default::default(), 600, 50).unwrap();
        seat.add_pointer();
        let output_state = smithay::wayland::output::OutputManagerState::new_with_xdg_output::<Self>(
            display_handle,
        );
        let output = smithay::output::Output::new(
            "winit".to_string(),  
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "Smithay".into(),
                model: "Winit".into(),
                serial_number: "0000".into(),
            },
        );
        let _global = output.create_global::<Self>(display_handle);
        let mode = smithay::output::Mode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(smithay::utils::Transform::Normal),
            Some(smithay::output::Scale::Fractional(scale_factor)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        Self {
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            seat,
            data_device_state: smithay::wayland::selection::data_device::DataDeviceState::new::<Self>(display_handle),
            xdg_decoration_state: XdgDecorationState::new::<Self>(display_handle),
            output_state,
            output,
            toplevels: Vec::new(),
            layout: Layout::new((width as f64 / scale_factor) as i32, (height as f64 / scale_factor) as i32),
            surface_buffers: std::collections::HashMap::new(),
            surface_positions: std::collections::HashMap::new(),
            drag_state: None,
            start_drag_request: None,
            loop_signal,
            width,
            height,
            scale_factor,
        }
    }
    pub fn update_scale_factor(&mut self, scale: f64) {
        log::info!("Updating Scale Factor to {}", scale);
        self.output.change_current_state(
            None,
            None,
            Some(smithay::output::Scale::Fractional(scale)),
            None,
        );
    }
}
impl smithay::wayland::output::OutputHandler for AppState {}
smithay::delegate_output!(AppState);
delegate_compositor!(AppState);
delegate_shm!(AppState);
delegate_seat!(AppState);
smithay::delegate_xdg_shell!(AppState);
impl CompositorHandler for AppState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        let client_data = client
            .get_data::<ClientState>()
            .expect("Client data missing");
        &client_data.compositor_state
    }
    fn new_surface(&mut self, surface: &WlSurface) {
        smithay::wayland::compositor::add_pre_commit_hook(surface, |_: &mut Self, _, surface| {
            smithay::wayland::compositor::with_states(surface, |states| {
                let mut guard = states
                    .cached_state
                    .get::<smithay::wayland::compositor::SurfaceAttributes>();
                log::info!(
                    "PRE-COMMIT HOOK: Surface {:?} Pending Buffer: {:?}",
                    surface.id(),
                    guard.pending().buffer
                );
            });
        });
    }
    fn commit(&mut self, surface: &WlSurface) {
        use smithay::wayland::compositor::{with_states, BufferAssignment, SurfaceAttributes};
        with_states(surface, |states| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            log::info!(
                "Commit Start: ID={:?}, Pending Buffer={:?}",
                surface.id(),
                guard.pending().buffer
            );
        });
        let _ = with_states(surface, |states| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            if let Some(buff) = &guard.current().buffer {
                log::info!("Commit End: Buffer IS PRESENT in current state: {:?}", buff);
            } else {
                log::info!("Commit End: Buffer is STILL NONE in current state!");
            }
        });
        with_states(surface, |states| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            {
                let pending = guard.pending();
                log::info!("PENDING buffer: {:?}", pending.buffer);
            }
        });
        let buffer_update = with_states(surface, |states| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            {
                let attributes = guard.current();
                log::info!("CURRENT buffer: {:?}", attributes.buffer);
                match &attributes.buffer {
                    Some(BufferAssignment::NewBuffer(buffer)) => {
                        log::info!(
                            "Buffer committed for surface {:?}, ID: {:?}",
                            surface.id(),
                            buffer.id()
                        );
                        Some(Some(buffer.clone()))
                    }
                    Some(BufferAssignment::Removed) => {
                        log::info!("Buffer removed for surface {:?}", surface.id());
                        Some(None)
                    }
                    None => {
                        log::info!(
                            "Commit without buffer change for surface {:?}",
                            surface.id()
                        );
                        None
                    }
                }
            }
        });
        if let Some(update) = buffer_update {
            let id = smithay::reexports::wayland_server::Resource::id(surface);
            if let Some(buffer) = update {
                self.surface_buffers.insert(id, buffer);
            } else {
                self.surface_buffers.remove(&id);
            }
        }
    }
}
impl XdgShellHandler for AppState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    fn new_toplevel(&mut self, surface: smithay::wayland::shell::xdg::ToplevelSurface) {
        log::info!("New XDG Toplevel Created: {:?}", surface.wl_surface().id());
        if !self.toplevels.contains(&surface) {
            self.toplevels.push(surface.clone());
            self.layout.add_tile(surface.clone());
            log::info!("Added tile to layout, now {} tiles", self.layout.tiles.len());
        }
        surface.with_pending_state(|state| {
            state.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated);
        });
        surface.send_configure();
    }
    fn new_popup(
        &mut self,
        _surface: smithay::wayland::shell::xdg::PopupSurface,
        _positioner: smithay::wayland::shell::xdg::PositionerState,
    ) {
    }
    fn grab(
        &mut self,
        _surface: smithay::wayland::shell::xdg::PopupSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
    }
    fn reposition_request(
        &mut self,
        _surface: smithay::wayland::shell::xdg::PopupSurface,
        _positioner: smithay::wayland::shell::xdg::PositionerState,
        _token: u32,
    ) {
    }
    fn maximize_request(&mut self, surface: smithay::wayland::shell::xdg::ToplevelSurface) {
        println!("*** HIT MAXIMIZE REQUEST ***");
        log::info!("Maximize Request: {:?}", surface.wl_surface().id());
        log::info!(
            "DEBUG MAXIMIZE: self.width={}, self.height={}, self.scale_factor={}",
            self.width,
            self.height,
            self.scale_factor
        );
        let logical_w = (self.width as f64 / self.scale_factor) as i32;
        let logical_h = (self.height as f64 / self.scale_factor) as i32;
        log::info!(
            "Maximizing to Logical Size: {}x{} (Physical: {}x{}, Scale: {})",
            logical_w,
            logical_h,
            self.width,
            self.height,
            self.scale_factor
        );
        surface.with_pending_state(|state| {
            state.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Maximized);
            state.size = Some((logical_w, logical_h).into());
        });
        surface.send_configure();
        let _ = self
            .loop_signal
            .send(crate::messages::CompositorMessage::Maximize(true));
    }
    fn unmaximize_request(&mut self, surface: smithay::wayland::shell::xdg::ToplevelSurface) {
        log::info!("Unmaximize Request: {:?}", surface.wl_surface().id());
        surface.with_pending_state(|state| {
             state.states.unset(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Maximized);
         });
        surface.send_configure();
        let _ = self
            .loop_signal
            .send(crate::messages::CompositorMessage::Maximize(false));
    }
    fn fullscreen_request(
        &mut self,
        surface: smithay::wayland::shell::xdg::ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        log::info!("Fullscreen Request: {:?}", surface.wl_surface().id());
        let logical_w = (self.width as f64 / self.scale_factor) as i32;
        let logical_h = (self.height as f64 / self.scale_factor) as i32;
        log::info!("Fullscreening to Logical Size: {}x{}", logical_w, logical_h);
        surface.with_pending_state(|state| {
             state.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen);
             state.size = Some((logical_w, logical_h).into());
         });
        surface.send_configure();
        let _ = self
            .loop_signal
            .send(crate::messages::CompositorMessage::Fullscreen(true));
    }
    fn unfullscreen_request(&mut self, surface: smithay::wayland::shell::xdg::ToplevelSurface) {
        log::info!("Unfullscreen Request: {:?}", surface.wl_surface().id());
        surface.with_pending_state(|state| {
             state.states.unset(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen);
        });
        surface.send_configure();
        let _ = self
            .loop_signal
            .send(crate::messages::CompositorMessage::Fullscreen(false));
    }
    fn move_request(
        &mut self,
        surface: smithay::wayland::shell::xdg::ToplevelSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
        log::info!(
            "XDG Move Request received for surface {:?}",
            surface.wl_surface().id()
        );
        let id = surface.wl_surface().id();
        self.start_drag_request = Some(id);
    }
}
impl ShmHandler for AppState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
impl BufferHandler for AppState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {
    }
}
impl SeatHandler for AppState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<AppState> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {
    }
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focus: Option<&Self::KeyboardFocus>) {}
}
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}
impl smithay::reexports::wayland_server::backend::ClientData for ClientState {
    fn initialized(&self, _client_id: smithay::reexports::wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        _client_id: smithay::reexports::wayland_server::backend::ClientId,
        _reason: smithay::reexports::wayland_server::backend::DisconnectReason,
    ) {
    }
}
use smithay::wayland::selection::data_device::DataDeviceState;
impl SelectionHandler for AppState {
    type SelectionUserData = ();
}
impl DataDeviceHandler for AppState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}
impl WaylandDndGrabHandler for AppState {}
delegate_data_device!(AppState);
use smithay::delegate_xdg_decoration;
use smithay::wayland::shell::xdg::ToplevelSurface;
impl XdgDecorationHandler for AppState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
        log::info!("New decoration requested - using server-side");
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
        toplevel.send_configure();
        log::info!("Decoration mode requested: {:?}", mode);
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
        log::info!("Decoration mode unset - defaulting to server-side");
    }
}
delegate_xdg_decoration!(AppState);