//! macOS native app-menu integration.
//!
//! Installs:  [Cocoa-Way]  [Connections ▾]
//! into NSApplication's main menu.
//!
//! "Connections" menu:
//!   - "Connect to Machine…"  → shows a quick-connect dialog
//!   - separator
//!   - saved connections from ~/.config/cocoa-way/connections.toml

use std::sync::Mutex;
use std::sync::mpsc::Sender;

use objc2::declare_class;
use objc2::mutability::MainThreadOnly;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{msg_send, msg_send_id, sel, ClassType, DeclaredClass};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSRect, NSString};

use crate::connections::Connection;
use crate::messages::CompositorMessage;

// ── Global channel sender ─────────────────────────────────────────────────────
static SENDER: Mutex<Option<Sender<CompositorMessage>>> = Mutex::new(None);

fn send(msg: CompositorMessage) {
    if let Ok(g) = SENDER.lock() {
        if let Some(tx) = g.as_ref() {
            let _ = tx.send(msg);
        }
    }
}

// ── ObjC handler class ────────────────────────────────────────────────────────
declare_class!(
    pub struct MenuHandler;

    unsafe impl ClassType for MenuHandler {
        type Super = NSObject;
        type Mutability = MainThreadOnly;
        const NAME: &'static str = "CocoaWayMenuHandler";
    }

    impl DeclaredClass for MenuHandler {
        type Ivars = ();
    }

    unsafe impl MenuHandler {
        /// "Connect to Machine…" — shows a quick-connect NSAlert dialog.
        #[method(quickConnect:)]
        fn quick_connect(&self, _sender: &AnyObject) {
            unsafe { show_quick_connect_dialog(); }
        }

        /// Connects to a saved machine. The NSMenuItem tag = index into connections list.
        #[method(connectMachine:)]
        fn connect_machine(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            send(CompositorMessage::Connect(tag as usize));
        }

        /// Toggles HiDPI rendering.
        #[method(toggleHiDpi:)]
        fn toggle_hidpi(&self, sender: &AnyObject) {
            let cur: isize = unsafe { msg_send![sender, state] };
            let next: isize = if cur == 1 { 0 } else { 1 };
            unsafe { let _: () = msg_send![sender, setState: next]; }
            send(CompositorMessage::ToggleHiDpi);
        }
    }
);

// ── Quick-connect dialog ──────────────────────────────────────────────────────

/// Shows an NSAlert with two text fields: user@host and program.
/// Blocks until dismissed. On "Connect", spawns waypipe.
unsafe fn show_quick_connect_dialog() {
    use objc2_app_kit::{NSAlert, NSTextField, NSSecureTextField, NSView};
    use objc2_foundation::NSRect;

    let alert: Retained<NSAlert> = msg_send_id![NSAlert::class(), new];
    let _: () = msg_send![&*alert, setMessageText: &*NSString::from_str("Connect to Remote Machine")];
    let _: () = msg_send![&*alert, setInformativeText:
        &*NSString::from_str("Enter the SSH host and the Wayland app to launch.")];

    // Accessory view: 300×116 containing three text fields (host / password / app)
    // NSView origin is bottom-left, so y increases upward.
    let frame = NSRect { origin: objc2_foundation::NSPoint { x: 0.0, y: 0.0 },
                          size:   objc2_foundation::NSSize  { width: 300.0, height: 116.0 } };
    let view: Retained<NSView> = msg_send_id![
        msg_send_id![NSView::class(), alloc],
        initWithFrame: frame
    ];

    // Top field: user@host
    let host_frame = NSRect {
        origin: objc2_foundation::NSPoint { x: 0.0, y: 80.0 },
        size:   objc2_foundation::NSSize  { width: 300.0, height: 28.0 },
    };
    let host_field: Retained<NSTextField> = msg_send_id![
        msg_send_id![NSTextField::class(), alloc],
        initWithFrame: host_frame
    ];
    let _: () = msg_send![&*host_field, setPlaceholderString:
        &*NSString::from_str("user@hostname-or-IP")];

    // Middle field: password (masked)
    let pass_frame = NSRect {
        origin: objc2_foundation::NSPoint { x: 0.0, y: 44.0 },
        size:   objc2_foundation::NSSize  { width: 300.0, height: 28.0 },
    };
    let pass_field: Retained<NSSecureTextField> = msg_send_id![
        msg_send_id![NSSecureTextField::class(), alloc],
        initWithFrame: pass_frame
    ];
    let _: () = msg_send![&*pass_field, setPlaceholderString:
        &*NSString::from_str("Password (leave blank to use SSH key)")];

    // Bottom field: app to launch
    let prog_frame = NSRect {
        origin: objc2_foundation::NSPoint { x: 0.0, y: 8.0 },
        size:   objc2_foundation::NSSize  { width: 300.0, height: 28.0 },
    };
    let prog_field: Retained<NSTextField> = msg_send_id![
        msg_send_id![NSTextField::class(), alloc],
        initWithFrame: prog_frame
    ];
    let _: () = msg_send![&*prog_field, setPlaceholderString:
        &*NSString::from_str("App to launch (e.g. weston-terminal)")];

    let _: () = msg_send![&*view, addSubview: &*host_field];
    let _: () = msg_send![&*view, addSubview: &*pass_field];
    let _: () = msg_send![&*view, addSubview: &*prog_field];
    let _: () = msg_send![&*alert, setAccessoryView: &*view];

    let _: Retained<NSObject> = msg_send_id![&*alert, addButtonWithTitle:
        &*NSString::from_str("Connect")];
    let _: Retained<NSObject> = msg_send_id![&*alert, addButtonWithTitle:
        &*NSString::from_str("Cancel")];

    // Make the first field the initial responder
    let _: () = msg_send![&*alert, layout];
    let win: Retained<NSObject> = msg_send_id![&*alert, window];
    let response: isize = msg_send![&*alert, runModal];
    // NSAlertFirstButtonReturn = 1000
    if response == 1000 {
        let host_ns: Retained<NSString> = msg_send_id![&*host_field, stringValue];
        let pass_ns: Retained<NSString> = msg_send_id![&*pass_field, stringValue];
        let prog_ns: Retained<NSString> = msg_send_id![&*prog_field, stringValue];
        let host_str = host_ns.to_string();
        let pass_str = pass_ns.to_string();
        let prog_str = prog_ns.to_string();
        if !host_str.is_empty() {
            let (user, host_addr) = if let Some(idx) = host_str.find('@') {
                (Some(host_str[..idx].to_string()), host_str[idx+1..].to_string())
            } else {
                (None, host_str.clone())
            };
            log::info!("Quick-connect: {} app={}", host_str, prog_str);
            let conn = crate::connections::Connection {
                name:      host_str,
                conn_type: "ssh".to_string(),
                host:      Some(host_addr),
                user,
                port:      None,
                identity:  None,
                socket:    None,
                app:       if prog_str.is_empty() { None } else { Some(prog_str) },
                password:  if pass_str.is_empty() { None } else { Some(pass_str) },
                waypipe_path: None,
            };
            let rt   = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
            let disp = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
            crate::connections::spawn_waypipe(&conn, &rt, &disp);
        }
    }
}

// ── Helper ────────────────────────────────────────────────────────────────────

unsafe fn label_item(title: &str, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    NSMenuItem::initWithTitle_action_keyEquivalent(
        mtm.alloc::<NSMenuItem>(),
        &NSString::from_str(title),
        None,
        &NSString::from_str(""),
    )
}

// ── Public setup ──────────────────────────────────────────────────────────────

/// Build and install the custom NSApplication main menu.
/// Must be called on the main thread after winit's `applicationDidFinishLaunching`.
pub fn setup_menu(
    connections: &[Connection],
    sender: Sender<CompositorMessage>,
    mtm: MainThreadMarker,
) {
    *SENDER.lock().unwrap() = Some(sender);

    unsafe {
        let handler: Retained<MenuHandler> = msg_send_id![MenuHandler::class(), new];

        let app  = NSApplication::sharedApplication(mtm);
        let root = NSMenu::new(mtm);

        // ── 1. App menu ("Cocoa-Way") ─────────────────────────────────────────
        let app_item = NSMenuItem::new(mtm);
        let app_menu = NSMenu::new(mtm);
        let quit = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc::<NSMenuItem>(),
            &NSString::from_str("Quit Cocoa-Way"),
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        );
        app_menu.addItem(&quit);
        app_item.setSubmenu(Some(&app_menu));
        root.addItem(&app_item);

        // ── 2. Connections menu ───────────────────────────────────────────────
        let conn_item = label_item("Connections", mtm);
        let conn_menu = NSMenu::initWithTitle(
            mtm.alloc::<NSMenu>(),
            &NSString::from_str("Connections"),
        );

        // "Connect to Machine…" dialog item (always present)
        let quick = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc::<NSMenuItem>(),
            &NSString::from_str("Connect to Machine…"),
            Some(sel!(quickConnect:)),
            &NSString::from_str("n"),
        );
        let _: () = msg_send![&*quick, setTarget: &*handler];
        conn_menu.addItem(&quick);
        conn_menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Saved connections
        if connections.is_empty() {
            let ph = label_item("No saved connections — use 'Connect to Machine…'", mtm);
            let _: () = msg_send![&*ph, setEnabled: false];
            conn_menu.addItem(&ph);
        } else {
            for (i, conn) in connections.iter().enumerate() {
                let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    mtm.alloc::<NSMenuItem>(),
                    &NSString::from_str(&conn.name),
                    Some(sel!(connectMachine:)),
                    &NSString::from_str(""),
                );
                let _: () = msg_send![&*item, setTag: i as isize];
                let _: () = msg_send![&*item, setTarget: &*handler];
                conn_menu.addItem(&item);
            }
        }
        conn_item.setSubmenu(Some(&conn_menu));
        root.addItem(&conn_item);

        // ── 3. View menu ──────────────────────────────────────────────────────
        let view_item = label_item("View", mtm);
        let view_menu = NSMenu::initWithTitle(
            mtm.alloc::<NSMenu>(),
            &NSString::from_str("View"),
        );
        let hidpi = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc::<NSMenuItem>(),
            &NSString::from_str("HiDPI Display"),
            Some(sel!(toggleHiDpi:)),
            &NSString::from_str(""),
        );
        let _: () = msg_send![&*hidpi, setState: 0isize];  // unchecked: Normal 1x is default
        let _: () = msg_send![&*hidpi, setTarget: &*handler];
        view_menu.addItem(&hidpi);
        view_item.setSubmenu(Some(&view_menu));
        root.addItem(&view_item);

        app.setMainMenu(Some(&root));
        std::mem::forget(handler);
    }
}

/// Disable macOS window tab bar (removes "Show Tab Bar" from the View menu).
/// Call this once after the winit window is created.
pub fn disable_window_tabbing(ns_window_ptr: *mut std::ffi::c_void) {
    if ns_window_ptr.is_null() { return; }
    unsafe {
        let win = ns_window_ptr as *mut AnyObject;
        // NSWindowTabbingModeDisallowed = 2
        let _: () = msg_send![win, setTabbingMode: 2isize];
    }
}
