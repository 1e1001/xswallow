//! X server atoms storage

use std::cell::UnsafeCell;
use std::ops::Deref;
use std::sync::Once;

use xcb::Connection;
use xcb::x::{self, Atom};

pub struct Ref(UnsafeCell<Atom>);
// SAFETY: only initialized once and read-only
unsafe impl Sync for Ref {}

impl Deref for Ref {
	type Target = Atom;
	fn deref(&self) -> &Self::Target {
		assert!(INIT_ATOMS.is_completed(), "atom used before init");
		// SAFETY: value initialized by `init_stub`
		unsafe { &*self.0.get() }
	}
}

pub static INIT_ATOMS: Once = Once::new();

pub fn init(connection: &Connection) {
	INIT_ATOMS.call_once(|| {
		// SAFETY: only called once ever, values cannot be read before this runs
		unsafe { init_stub(connection) };
	});
}

macro_rules! generate {
	($($id:ident)*) => {
		$(//#[allow(non_snake_case, reason = "atom identifier")]
		pub static $id: Ref = Ref((UnsafeCell::new(x::ATOM_NONE)));)*
		unsafe fn init_stub(connection: &Connection) {
			#[expect(non_snake_case, reason = "macro")]
			struct Events {$(
				$id: x::InternAtomCookie,
			)*}
			let events = Events {$(
				$id: connection.send_request(&x::InternAtom {
					only_if_exists: false,
					name: stringify!($id).as_bytes(),
				}),
			)*};
			$(*$id.0.get() = connection.wait_for_reply(events.$id).expect("Failed to load atom").atom();)*
		}
	};
}

generate! {
	_NET_ACTIVE_WINDOW
	_NET_CLIENT_LIST
	_NET_WM_PID
	_NET_WM_DESKTOP
	// from ICCCM, not a typo
	WM_CHANGE_STATE
	_NET_WM_STATE
	_NET_WM_STATE_MAXIMIZED_VERT
	_NET_WM_STATE_MAXIMIZED_HORZ
	_NET_WM_STATE_STICKY
	_NET_WM_STATE_SHADED
	_NET_WM_STATE_HIDDEN
	_NET_WM_STATE_FULLSCREEN
	_NET_WM_STATE_ABOVE
	_NET_WM_STATE_BELOW
}
