use log::info;
use std::sync::Arc;
use winit::event::{ElementState, Event, KeyEvent, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::{Display, ListeningSocket};
use smithay::utils::SERIAL_COUNTER;
mod keymap;
mod layout;
mod messages;
mod render;
mod state;
mod metal_renderer;
mod connections;
mod menu_bar;

use messages::CompositorMessage;
use crate::state::AppState;
fn main() {
    // Default filter: our code at INFO, smithay/wayland noise at WARN only.
    // Override with RUST_LOG env var.
    let filter = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "cocoa_way=info,smithay=warn,wayland_server=warn,wayland_client=warn".into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();
    let event_loop = EventLoop::new().unwrap();
    // Build the window first, then hand it to the Metal renderer.
    let window = winit::window::WindowBuilder::new()
        .with_title("Cocoa-Way")
        .with_inner_size(winit::dpi::LogicalSize::new(800.0f64, 600.0f64))
        .build(&event_loop)
        .expect("Failed to create window");
    let mut renderer = metal_renderer::MetalRenderer::new(window)
        .expect("Failed to create MetalRenderer");
    info!("MetalRenderer created with Metal hardware rendering");
    let mut display = Display::<AppState>::new().unwrap();
    let display_handle = display.handle();
    let (loop_signal, loop_receiver) = std::sync::mpsc::channel::<CompositorMessage>();
    let menu_signal = loop_signal.clone(); // separate sender for the menu bar
    // Use scale=1: clients render at physical pixel resolution (1600x1200).
    // This gives pixel-perfect 1:1 rendering instead of blurry 2x upscale.
    let mut state = AppState::new(
        &display_handle,
        1.0,   // compositor scale=1: layout in physical pixels
        loop_signal,
        renderer.window.inner_size().width,
        renderer.window.inner_size().height,
    );
    let initial_size = renderer.window.inner_size();
    let initial_mode = smithay::output::Mode {
        size: (initial_size.width as i32, initial_size.height as i32).into(),
        refresh: 60_000,
    };
    state.output.change_current_state(
        Some(initial_mode),
        Some(smithay::utils::Transform::Normal),
        Some(smithay::output::Scale::Integer(1)),
        Some((0, 0).into()),
    );
    state.output.set_preferred(initial_mode);
    let runtime_dir = std::env::temp_dir().join("cocoa-way");
    if !runtime_dir.exists() {
        std::fs::create_dir_all(&runtime_dir).unwrap();
    }
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir); }
    let listener = ListeningSocket::bind_auto("wayland", 1..10).unwrap();
    let socket_name = listener
        .socket_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let socket_path = runtime_dir.join(&socket_name);
    info!("Wayland socket created: {:?}", socket_name);
    info!("XDG_RUNTIME_DIR set to: {:?}", runtime_dir);
    info!(
        "To run clients: export XDG_RUNTIME_DIR={:?} WAYLAND_DISPLAY={}",
        runtime_dir, socket_name
    );
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name); }
    let mut loop_handle = display_handle.clone();
    std::thread::spawn(move || loop {
        match listener.accept() {
            Ok(Some(stream)) => {
                use crate::state::ClientState;
                info!("New client connected");
                loop_handle
                    .insert_client(
                        stream,
                        Arc::new(ClientState {
                            compositor_state: Default::default(),
                        }),
                    )
                    .unwrap();
            }
            Ok(None) => {}
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    });
    let runtime_dir_str = runtime_dir.to_string_lossy().into_owned();
    let mut hidpi_enabled = false;

    // Will be installed in Event::Resumed (after winit's applicationDidFinishLaunching)
    let connections_for_menu = connections::load_connections();
    let mut pending_menu: Option<std::sync::mpsc::Sender<CompositorMessage>> = Some(menu_signal);

    let mut last_mouse_pos =
        smithay::utils::Point::<f64, smithay::utils::Logical>::from((0.0, 0.0));
    let start_time = std::time::Instant::now();
    let frame_duration = std::time::Duration::from_millis(16); // ~60fps cap
    let mut last_frame = std::time::Instant::now();
    let mut last_layout_size: (i32, i32) = (0, 0); // track last logical size sent to layout
    event_loop.run(move |event, target| {
        while let Ok(msg) = loop_receiver.try_recv() {
            match msg {
                CompositorMessage::Maximize(max) => {
                    log::info!("Handling Maximize: {}", max);
                    renderer.window.set_maximized(max);
                },
                CompositorMessage::Fullscreen(full) => {
                    log::info!("Handling Fullscreen: {}", full);
                    if full {
                        renderer.window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
                    } else {
                        renderer.window.set_fullscreen(None);
                    }
                }
                CompositorMessage::ToggleHiDpi => {
                    hidpi_enabled = !hidpi_enabled;
                    // Two modes:
                    //  • HiDPI (scale=2): configure clients at logical (800×600).
                    //    HiDPI-aware clients render 1600×1200 at buf_scale=2 → 1:1 sharp.
                    //  • Normal (scale=1): configure clients at physical (1600×1200).
                    //    All clients render 1600×1200 at buf_scale=1 → 1:1 sharp.
                    let sys_scale = renderer.window.scale_factor();
                    let new_scale = if hidpi_enabled { sys_scale } else { 1.0 };
                    state.scale_factor = new_scale;
                    // Advertise new output scale to clients.
                    state.output.change_current_state(
                        None, None,
                        Some(smithay::output::Scale::Integer(new_scale.round() as i32)),
                        None,
                    );
                    // Recalculate layout for new logical viewport.
                    let log_w = (state.width as f64 / new_scale) as i32;
                    let log_h = (state.height as f64 / new_scale) as i32;
                    state.layout.set_view_size(log_w, log_h);
                    // Relayout sends new configure to every client.
                    for tile in state.layout.tiles.iter() {
                        tile.request_size();
                    }
                    renderer.request_redraw();
                    log::info!("Mode: {} (compositor scale={}, logical={}x{})",
                        if hidpi_enabled { "HiDPI 2x" } else { "Normal 1x" },
                        new_scale as i32, log_w, log_h);
                }
                CompositorMessage::Connect(i) => {
                    log::info!("Connecting to machine #{}", i);
                    if let Some(conn) = connections::load_connections().get(i) {
                        let rt = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
                        let disp = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
                        connections::spawn_waypipe(conn, &rt, &disp);
                    }
                }
            }
        }
        match event {
            Event::WindowEvent { window_id, event } if window_id == renderer.window.id() => {
                match event {
                     WindowEvent::Resized(size) => {
                         println!("*** HIT RESIZED EVENT: {}x{} ***", size.width, size.height);
                         let width = size.width as i32;
                         let height = size.height as i32;
                         renderer.resize(size.width, size.height);
                         state.width = size.width;
                         state.height = size.height;
                         // Preserve whatever scale mode was active (HiDPI or normal).
                         let cur_scale = state.scale_factor;
                         let mode = smithay::output::Mode {
                             size: (width, height).into(),
                             refresh: 60_000,
                         };
                         state.output.change_current_state(
                             Some(mode),
                             Some(smithay::utils::Transform::Normal),
                             Some(smithay::output::Scale::Integer(cur_scale.round() as i32)),
                             Some((0,0).into())
                         );
                         // Recalculate layout and tell all clients their new size.
                         let log_w = (width as f64 / cur_scale) as i32;
                         let log_h = (height as f64 / cur_scale) as i32;
                         state.layout.set_view_size(log_w, log_h);
                         for tile in state.layout.tiles.iter() {
                             tile.request_size();
                         }
                     },
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        log::info!("ScaleFactorChanged: {}", scale_factor);
                        state.update_scale_factor(scale_factor);
                        renderer.set_scale_factor(scale_factor);
                    },
                    WindowEvent::CloseRequested => target.exit(),
                    WindowEvent::KeyboardInput { event: KeyEvent { state: el_state, physical_key, .. }, .. } => {
                        if let winit::keyboard::PhysicalKey::Code(key_code) = physical_key {
                            match key_code {
                                _ => {
                                     use smithay::backend::input::KeyState;
                                     use smithay::input::keyboard::Keycode;  
                                     let serial = SERIAL_COUNTER.next_serial();
                                     let time = start_time.elapsed().as_millis() as u32;
                                     if let Some(keyboard) = state.seat.get_keyboard() {
                                         if let Some(scancode) = crate::keymap::map_key(physical_key) {
                                             let key_state = match el_state {
                                                 ElementState::Pressed => KeyState::Pressed,
                                                 ElementState::Released => KeyState::Released,
                                             };
                                             let keycode = Keycode::from(scancode + 8);
                                             keyboard.input(&mut state, keycode, key_state, serial, time, |_, _, _| FilterResult::<()>::Forward);
                                         }
                                     }
                                }
                            }
                        }
                    },
                    WindowEvent::CursorMoved { position, .. } => {
                        let scale = state.scale_factor;
                        let logical_pos = position.to_logical::<f64>(scale);
                        log::debug!("CursorMoved: Physical({:?}) -> Logical({:?})", position, logical_pos);
                        let serial = SERIAL_COUNTER.next_serial();
                        let pointer = state.seat.get_pointer().unwrap();
                        let position_f64 = smithay::utils::Point::<f64, smithay::utils::Logical>::from((logical_pos.x, logical_pos.y));
                         if let Some(target_id) = state.start_drag_request.take() {
                              let (cur_x, cur_y) = state.layout.tile_for_surface(&target_id)
                                  .map(|t| (t.position.x, t.position.y))
                                  .unwrap_or((0, 0));
                              let offset_x = logical_pos.x - cur_x as f64;
                              let offset_y = logical_pos.y - cur_y as f64;
                              state.drag_state = Some((target_id.clone(), (offset_x, offset_y)));
                              log::info!("Drag Started for {:?}", target_id);
                         }
                        if let Some((target_id, (offset_x, offset_y))) = state.drag_state.clone() {
                            let new_x = (logical_pos.x - offset_x) as i32;
                            let new_y = (logical_pos.y - offset_y) as i32;
                            state.layout.move_tile(&target_id, new_x, new_y);
                            renderer.request_redraw();
                        }
                        let mut focus = None;
                         let cursor_logical_point = smithay::utils::Point::<f64, smithay::utils::Logical>::from((logical_pos.x, logical_pos.y));
                         last_mouse_pos = cursor_logical_point;
                         for tile in state.layout.tiles.iter().rev() {
                             let tile_x = tile.position.x as f64;
                             let tile_y = tile.position.y as f64;
                             let tile_w = tile.size.w as f64;
                             let tile_h = tile.size.h as f64;
                             if logical_pos.x >= tile_x && logical_pos.x < tile_x + tile_w
                                && logical_pos.y >= tile_y && logical_pos.y < tile_y + tile_h {
                                 let wl_surface = tile.toplevel.wl_surface();
                                 let surface_location = smithay::utils::Point::<f64, smithay::utils::Logical>::from(
                                     (tile_x, tile_y)
                                 );
                                 log::debug!("HitTest: FOUND tile {:?} at logical ({:.0}, {:.0})", wl_surface.id(), tile_x, tile_y);
                                 focus = Some((wl_surface.clone(), surface_location));
                                 break;
                             }
                         }
                         if focus.is_none() && !state.layout.tiles.is_empty() {
                             log::debug!("HitTest: cursor at ({:.0}, {:.0}) not in any tile", logical_pos.x, logical_pos.y);
                         }
                         let time = start_time.elapsed().as_millis() as u32;
                        // Send relative motion if the focused surface has an active lock constraint.
                        let delta = position_f64 - last_mouse_pos;
                        let is_locked = focus.as_ref().map(|(surface, _)| {
                            smithay::wayland::pointer_constraints::with_pointer_constraint::<crate::state::AppState, _, _>(
                                surface,
                                &pointer,
                                |constraint| {
                                    constraint.map(|c| {
                                        matches!(*c, smithay::wayland::pointer_constraints::PointerConstraint::Locked(_)) && c.is_active()
                                    }).unwrap_or(false)
                                },
                            )
                        }).unwrap_or(false);
                        if delta.x != 0.0 || delta.y != 0.0 {
                            pointer.relative_motion(
                                &mut state,
                                focus.clone(),
                                &smithay::input::pointer::RelativeMotionEvent {
                                    delta,
                                    delta_unaccel: delta,
                                    utime: time as u64 * 1000,
                                },
                            );
                        }
                        if !is_locked {
                            let event = MotionEvent {
                                location: cursor_logical_point,
                                serial,
                                time,
                            };
                            pointer.motion(&mut state, focus, &event);
                        }
                        pointer.frame(&mut state);
                    },
                    WindowEvent::MouseInput { state: el_state, button, .. } => {
                        log::info!("MouseInput: {:?} {:?}", button, el_state);
                        let serial = SERIAL_COUNTER.next_serial();
                        let pointer = state.seat.get_pointer().unwrap();
                        let keyboard = state.seat.get_keyboard().unwrap();
                        let button_code = match button {
                            winit::event::MouseButton::Left => 0x110,  
                            winit::event::MouseButton::Right => 0x111,
                            winit::event::MouseButton::Middle => 0x112,
                            _ => 0x110,
                        };
                         let p_state = match el_state {
                            ElementState::Pressed => smithay::backend::input::ButtonState::Pressed,
                            ElementState::Released => smithay::backend::input::ButtonState::Released,
                        };
                        let time = start_time.elapsed().as_millis() as u32;
                        if p_state == smithay::backend::input::ButtonState::Pressed && button == winit::event::MouseButton::Left {
                            let mut focus_surface = None;
                            if let Some(pointer_state) = state.seat.get_pointer() {
                                if let Some(surface) = pointer_state.current_focus() {
                                    focus_surface = Some(surface);
                                }
                            }
                            if let Some(surface) = focus_surface {
                                log::info!("Click-Focus: Setting keyboard focus to {:?}", surface.id());
                                keyboard.set_focus(&mut state, Some(surface.clone()), serial);
                                if let Some(toplevel) = state.toplevels.iter().find(|t| t.wl_surface() == &surface) {
                                     toplevel.with_pending_state(|state| {
                                        state.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated);
                                    });
                                    toplevel.send_configure();
                                }
                            } else {
                                keyboard.set_focus(&mut state, None, serial);
                            }
                        }
                         if p_state == smithay::backend::input::ButtonState::Pressed && button == winit::event::MouseButton::Left {
                             if let Some(target_id) = state.start_drag_request.take() {
                                 let (cur_x, cur_y) = state.layout.tile_for_surface(&target_id)
                                     .map(|t| (t.position.x, t.position.y))
                                     .unwrap_or((0, 0));
                                 let offset_x = last_mouse_pos.x - cur_x as f64;
                                 let offset_y = last_mouse_pos.y - cur_y as f64;
                                 state.drag_state = Some((target_id, (offset_x, offset_y)));
                             }
                         }
                        if p_state == smithay::backend::input::ButtonState::Released && button == winit::event::MouseButton::Left {
                            state.drag_state = None;
                        }
                        let event = ButtonEvent {
                            button: button_code,
                            state: p_state,
                            serial,
                            time,
                        };
                        pointer.button(&mut state, &event);
                        pointer.frame(&mut state);
                    },
                    WindowEvent::MouseWheel { delta, phase, .. } => {
                        let pointer = state.seat.get_pointer().unwrap();
                        let time = start_time.elapsed().as_millis() as u32;
                        let (idx, amount, source) = match delta {
                           winit::event::MouseScrollDelta::LineDelta(x, y) => {
                               if x != 0.0 {
                                   (smithay::backend::input::Axis::Horizontal, -x as f64 * 10.0, smithay::backend::input::AxisSource::Wheel)
                               } else {
                                   (smithay::backend::input::Axis::Vertical, -y as f64 * 10.0, smithay::backend::input::AxisSource::Wheel)
                               }
                           },
                           winit::event::MouseScrollDelta::PixelDelta(pos) => {
                               let logical_pos = pos.to_logical::<f64>(state.scale_factor);
                               if logical_pos.x != 0.0 {
                                   (smithay::backend::input::Axis::Horizontal, -logical_pos.x, smithay::backend::input::AxisSource::Finger)
                               } else {
                                   (smithay::backend::input::Axis::Vertical, -logical_pos.y, smithay::backend::input::AxisSource::Finger)
                               }
                           }
                        };
                        if amount != 0.0 {
                             let (h, v) = if idx == smithay::backend::input::Axis::Horizontal { (amount, 0.0) } else { (0.0, amount) };
                             let stop_tuple = if phase == winit::event::TouchPhase::Ended {
                                 if idx == smithay::backend::input::Axis::Horizontal { (true, false) } else { (false, true) }
                             } else { (false, false) };
                             let details = smithay::input::pointer::AxisFrame {
                                 source: Some(source),
                                 time,
                                 axis: (h, v),
                                 stop: stop_tuple,
                                 v120: Some((0, 0)),
                                 relative_direction: (smithay::backend::input::AxisRelativeDirection::Identical, smithay::backend::input::AxisRelativeDirection::Identical),
                             };
                             pointer.axis(&mut state, details);
                             pointer.frame(&mut state);
                        }
                    },
                    WindowEvent::RedrawRequested => {
                         let (width, height) = {
                            let size = renderer.window.inner_size();
                            (size.width, size.height)
                        };
                        if width > 0 && height > 0 {
                            if width != renderer.width || height != renderer.height {
                                renderer.resize(width, height);
                            }
                            renderer.clear(0.1, 0.1, 0.15, 1.0);
                            use smithay::reexports::wayland_server::Resource;
                            let mut rendered_count = 0;
                            let before_toplevels = state.toplevels.len();
                            let before_tiles = state.layout.tiles.len();
                            for tile in state.layout.tiles.iter() {
                                if !tile.toplevel.wl_surface().is_alive() {
                                    renderer.evict_texture(&tile.toplevel.wl_surface().id());
                                }
                            }
                            state.toplevels.retain(|t| t.wl_surface().is_alive());
                            state.layout.tiles.retain(|t| t.toplevel.wl_surface().is_alive());
                            if state.toplevels.len() != before_toplevels || state.layout.tiles.len() != before_tiles {
                                log::warn!("CLEANUP: toplevels {} -> {}, tiles {} -> {}", 
                                    before_toplevels, state.toplevels.len(),
                                    before_tiles, state.layout.tiles.len());
                            }
                            let scale = state.scale_factor;
                            let logical_width = (width as f64 / scale) as i32;
                            let logical_height = (height as f64 / scale) as i32;
                            if (logical_width, logical_height) != last_layout_size {
                                last_layout_size = (logical_width, logical_height);
                                state.layout.set_view_size(logical_width, logical_height);
                            }
                            if state.layout.tiles.is_empty() {
                                log::debug!("RENDER: No tiles to render");
                            } else {
                                log::debug!("RENDER: {} tiles", state.layout.tiles.len());
                            }
                            for tile in state.layout.tiles.iter() {
                                let wl_surface = tile.toplevel.wl_surface();
                                let id = wl_surface.id();
                                let x_offset = tile.position.x;
                                let y_offset = tile.position.y;
                                let phys_x = (x_offset as f64 * scale) as i32;
                                let phys_y = (y_offset as f64 * scale) as i32;
                                let phys_w = (tile.size.w as f64 * scale) as i32;
                                let phys_h = (tile.size.h as f64 * scale) as i32;
                                // Shadow removed — off-screen quads cause Metal triangle clipping artifacts.
                                smithay::wayland::compositor::with_surface_tree_downward(
                                    wl_surface,
                                    (x_offset, y_offset),
                                    |_, _, &loc| {
                                        smithay::wayland::compositor::TraversalAction::DoChildren(loc)
                                    },
                                    |surface, states, &loc| {
                                        let mut guard = states.cached_state.get::<smithay::wayland::compositor::SurfaceAttributes>();
                                        let current = guard.current();
                                        let viewport_dst = {
                                            let mut vg = states.cached_state.get::<smithay::wayland::viewporter::ViewportCachedState>();
                                            vg.current().dst
                                        };
                                        let phys_x = (loc.0 as f64 * scale) as i32;
                                        let phys_y = (loc.1 as f64 * scale) as i32;
                                        let surf_id = surface.id();
                                        match &current.buffer {
                                            Some(smithay::wayland::compositor::BufferAssignment::NewBuffer(b)) => {
                                                let buffer_scale = current.buffer_scale;
                                                let buf_id = b.id();
                                                if let Some((tex_w, tex_h)) = renderer.lookup_cached_size(&surf_id, &buf_id) {
                                                    let dest_w = viewport_dst.map(|d| (d.w as f64 * scale).round() as i32)
                                                        .unwrap_or_else(|| (tex_w as f64 / buffer_scale as f64 * scale).round() as i32);
                                                    let dest_h = viewport_dst.map(|d| (d.h as f64 * scale).round() as i32)
                                                        .unwrap_or_else(|| (tex_h as f64 / buffer_scale as f64 * scale).round() as i32);
                                                    renderer.draw_pixels(surf_id, buf_id, phys_x, phys_y, dest_w, dest_h, 0, 0, &[]);
                                                    rendered_count += 1;
                                                } else if let Some((buf_w, buf_h, pixels)) = crate::render::get_buffer_pixels(b) {
                                                    let dest_w = viewport_dst.map(|d| (d.w as f64 * scale).round() as i32)
                                                        .unwrap_or_else(|| (buf_w as f64 / buffer_scale as f64 * scale).round() as i32);
                                                    let dest_h = viewport_dst.map(|d| (d.h as f64 * scale).round() as i32)
                                                        .unwrap_or_else(|| (buf_h as f64 / buffer_scale as f64 * scale).round() as i32);
                                                    renderer.draw_pixels(surf_id, buf_id, phys_x, phys_y, dest_w, dest_h, buf_w, buf_h, &pixels);
                                                    rendered_count += 1;
                                                } else {
                                                    log::warn!("RENDER: unsupported buffer format for {:?} — not wl_shm (EGL/DMA-buf?); run with LIBGL_ALWAYS_SOFTWARE=1", surf_id);
                                                    // Still try cached texture from a previous frame if available
                                                    if renderer.draw_from_cache(&surf_id, phys_x, phys_y, scale, viewport_dst) {
                                                        rendered_count += 1;
                                                    }
                                                }
                                            },
                                            Some(smithay::wayland::compositor::BufferAssignment::Removed) => {
                                                log::debug!("RENDER: buffer removed for {:?}", surf_id);
                                                renderer.evict_texture(&surf_id);
                                            },
                                            None => {
                                                // No new buffer this commit — re-use the cached texture if present.
                                                if renderer.draw_from_cache(&surf_id, phys_x, phys_y, scale, viewport_dst) {
                                                    rendered_count += 1;
                                                }
                                            }
                                        }
                                    },
                                    |_, _, _| true
                                );
                                let is_focused = state.seat.get_keyboard()
                                    .and_then(|k| k.current_focus())
                                    .map(|s| &s == wl_surface)
                                    .unwrap_or(false);
                                let border_width = 4;
                                // Only draw border when tile has enough margin — same NDC
                                // clipping issue as shadow if we go negative.
                                if is_focused && phys_x >= border_width && phys_y >= border_width {
                                    renderer.draw_border(
                                        phys_x - border_width,
                                        phys_y - border_width,
                                        phys_w + border_width * 2,
                                        phys_h + border_width * 2,
                                        border_width as f32,
                                    );
                                }
                            }
                            // Render popups on top of toplevels
                            state.popups.retain(|p| p.wl_surface().is_alive());
                            let scale = state.scale_factor;
                            for popup in state.popups.iter() {
                                let parent_surface = match popup.get_parent_surface() {
                                    Some(s) => s,
                                    None => continue,
                                };
                                let parent_id = parent_surface.id();
                                let parent_pos = state.layout.tile_for_surface(&parent_id)
                                    .map(|t| (t.position.x, t.position.y))
                                    .unwrap_or((0, 0));
                                let popup_geo = smithay::wayland::compositor::with_states(
                                    popup.wl_surface(),
                                    |states| {
                                        let mut cached = states.cached_state
                                            .get::<smithay::wayland::shell::xdg::PopupCachedState>();
                                        cached.current().last_acked
                                            .as_ref()
                                            .map(|c| c.state.geometry)
                                    },
                                );
                                let geo = match popup_geo {
                                    Some(g) => g,
                                    None => continue,
                                };
                                let popup_log_x = parent_pos.0 + geo.loc.x;
                                let popup_log_y = parent_pos.1 + geo.loc.y;
                                smithay::wayland::compositor::with_surface_tree_downward(
                                    popup.wl_surface(),
                                    (popup_log_x, popup_log_y),
                                    |_, _, &loc| {
                                        smithay::wayland::compositor::TraversalAction::DoChildren(loc)
                                    },
                                    |surface, states, &loc| {
                                        let mut guard = states.cached_state.get::<smithay::wayland::compositor::SurfaceAttributes>();
                                        let current = guard.current();
                                        if let Some(smithay::wayland::compositor::BufferAssignment::NewBuffer(b)) = &current.buffer {
                                            let buffer_scale = current.buffer_scale;
                                            let px = (loc.0 as f64 * scale) as i32;
                                            let py = (loc.1 as f64 * scale) as i32;
                                            let surf_id = surface.id();
                                            let buf_id = b.id();
                                            if let Some((tex_w, tex_h)) = renderer.lookup_cached_size(&surf_id, &buf_id) {
                                                let dest_w = (tex_w as f64 / buffer_scale as f64 * scale).round() as i32;
                                                let dest_h = (tex_h as f64 / buffer_scale as f64 * scale).round() as i32;
                                                renderer.draw_pixels(surf_id, buf_id, px, py, dest_w, dest_h, 0, 0, &[]);
                                            } else if let Some((buf_w, buf_h, pixels)) = crate::render::get_buffer_pixels(b) {
                                                let dest_w = (buf_w as f64 / buffer_scale as f64 * scale).round() as i32;
                                                let dest_h = (buf_h as f64 / buffer_scale as f64 * scale).round() as i32;
                                                renderer.draw_pixels(surf_id, buf_id, px, py, dest_w, dest_h, buf_w, buf_h, &pixels);
                                            }
                                        }
                                    },
                                    |_, _, _| true,
                                );
                            }
                            if rendered_count == 0 && !state.layout.tiles.is_empty() {
                                log::warn!(
                                    "RENDER: {} tiles present but nothing rendered — likely unsupported buffer format or no committed buffer yet",
                                    state.layout.tiles.len()
                                );
                            }
                            if let Err(e) = renderer.swap_buffers() {
                                log::error!("Failed to swap buffers: {}", e);
                            }
                            let t = state.start_time.elapsed().as_millis() as u32;
                            for cb in state.pending_frame_callbacks.drain(..) {
                                cb.done(t);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::AboutToWait => {
                  match display.dispatch_clients(&mut state) {
                      Ok(_) => {
                          display.flush_clients().unwrap();
                      }
                      Err(_) => {}
                  }
                  let now = std::time::Instant::now();
                  if now.duration_since(last_frame) >= frame_duration {
                      renderer.request_redraw();
                      last_frame = now;
                  } else {
                      target.set_control_flow(ControlFlow::WaitUntil(
                          last_frame + frame_duration,
                      ));
                  }
            }
            Event::Resumed => {
                // Install menu bar once, after winit's applicationDidFinishLaunching.
                if let Some(sender) = pending_menu.take() {
                    // SAFETY: Resumed always fires on the main thread
                    let mtm = unsafe { objc2_foundation::MainThreadMarker::new_unchecked() };
                    menu_bar::setup_menu(&connections_for_menu, sender, mtm);
                    // Disable macOS tab bar via NSView -> NSWindow
                    {
                        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        if let Ok(handle) = renderer.window.window_handle() {
                            if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                                let ns_view = h.ns_view.as_ptr() as *mut objc2::runtime::AnyObject;
                                // -[NSView window] returns id (@), not *mut c_void (^v)
                                let ns_win: *mut objc2::runtime::AnyObject = unsafe {
                                    objc2::msg_send![ns_view, window]
                                };
                                if !ns_win.is_null() {
                                    menu_bar::disable_window_tabbing(ns_win as *mut std::ffi::c_void);
                                }
                            }
                        }
                    }
                    log::info!("macOS menu bar installed");
                }
            }
            _ => {}
        }
    }).unwrap();
}
