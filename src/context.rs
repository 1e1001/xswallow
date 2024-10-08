//! abstraction over xcb connection (and some relevant things)
//! all raw xcb code goes here
use std::collections::VecDeque;
use std::convert::Infallible;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::str::from_utf8;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, sync_channel};
use std::{array, fmt, iter, thread};

use xcb::x::{self, Atom, Window};
use xcb::{Connection, Xid};

use crate::output;

/// event sent from the poll thread
enum ThreadEvent {
	Quit,
	Err(xcb::Error),
	PropertyNotify(x::PropertyNotifyEvent),
	ConfigureNotify(x::ConfigureNotifyEvent),
	DestroyNotify(x::DestroyNotifyEvent),
	Other,
}

/// actual returned event
#[derive(Clone, Copy)]
pub enum Event {
	/// an unimportant event
	Interrupted,
	Quit,
	WindowList,
	Update(Window),
	Close(Window),
}

/// `_NET_WM_STATE` in a bitfield
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
struct WindowState(u8);
impl WindowState {
	fn new(cx: &Context, list: &[Atom]) -> Self {
		// TODO: there's a better way to do this match
		Self(list.iter().fold(0, |out, &i| {
			out | match i {
				i if i == cx.atom_state_max_vert => 0x01,
				i if i == cx.atom_state_max_horz => 0x02,
				i if i == cx.atom_state_sticky => 0x04,
				i if i == cx.atom_state_shaded => 0x08,
				i if i == cx.atom_state_hidden => 0x10,
				i if i == cx.atom_state_fullscreen => 0x20,
				i if i == cx.atom_state_above => 0x40,
				i if i == cx.atom_state_below => 0x80,
				_ => 0,
			}
		}))
	}
	fn take_one(&mut self) -> usize {
		let index = self.0.trailing_zeros();
		self.0 &= !1_u8.wrapping_shl(index);
		index as usize
	}
	/// list of state events needed to make a window enter this state.
	/// does not update the minimized state
	fn events(self, cx: &Context) -> [[u32; 5]; 4] {
		// TODO: write a proof for this
		// ∀n. ceil(n/2) + ceil((7-n)/2) = 4
		let atoms = [
			cx.atom_state_max_vert.resource_id(),
			cx.atom_state_max_horz.resource_id(),
			cx.atom_state_sticky.resource_id(),
			cx.atom_state_shaded.resource_id(),
			cx.atom_state_hidden.resource_id(),
			cx.atom_state_fullscreen.resource_id(),
			cx.atom_state_above.resource_id(),
			cx.atom_state_below.resource_id(),
			x::ATOM_NONE.resource_id(),
		];
		let mut next = Self(self.0 & !0x10);
		let mut state = 1;
		array::from_fn(|_| {
			if next.0 == 0 {
				next = Self(!self.0 & !0x10);
				state = 0;
			};
			[state, atoms[next.take_one()], atoms[next.take_one()], 2, 0]
		})
	}
	/// iterator of short property names
	#[cfg_attr(rust_analyzer, expect(unused_mut, reason = "rust-analyzer#18209"))]
	fn names(mut self) -> impl Iterator<Item = &'static str> {
		const ATOM_NAMES: &str = "+V+H+S-S-M+M+O-O";
		iter::from_fn(move || {
			let atom = self.take_one() * 2;
			ATOM_NAMES.get(atom..atom + 2)
		})
	}
	fn is_hidden(self) -> bool {
		self.0 & 0x10 != 0
	}
}

/// where a window is
#[derive(Clone, Copy)]
pub struct Geometry {
	x: i16,
	y: i16,
	w: u16,
	h: u16,
	d: u32,
	s: WindowState,
}

impl fmt::Display for Geometry {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let Self { x, y, w, h, d, s } = self;
		write!(f, "{w}x{h}+{x},{y}@{d}")?;
		for entry in s.names() {
			write!(f, "{entry}")?;
		}
		Ok(())
	}
}

// so i don't need to write out atom_ a bunch of times
macro_rules! intern {
	($Context:ident $connection:ident $rx:ident $root:ident $new:ident, $($var:ident = $name:literal,)*) => {
		pub struct $Context {
			$connection: Arc<Connection>,
			$rx: Receiver<ThreadEvent>,
			$root: Window,
			$($var: Atom,)*
		}
		impl $Context {
			pub fn $new() -> xcb::Result<Self> {
				let ($connection, screen) = Connection::connect(None)?;
				let $connection = Arc::new($connection);
				let $root = $connection
					.get_setup()
					.roots()
					.nth(usize::try_from(screen).unwrap_or_default())
					.expect("No screen")
					.root();
				$connection.send_request(&x::ChangeWindowAttributes {
					window: $root,
					value_list: &[x::Cw::EventMask(x::EventMask::PROPERTY_CHANGE)],
				});
				$(let $var = $connection.send_request(&x::InternAtom {
					only_if_exists: false,
					name: $name.as_bytes(),
				});)*
				$(let $var = $connection.wait_for_reply($var)?.atom();)*
				let $rx = event_thread(Arc::clone(&$connection));
				output::setup_context(screen, $root, &[$($var,)*]);
				Ok(Self {
					$connection,
					$rx,
					$root,
					$($var,)*
				})
			}
		}
	}
}
intern!(
	Context connection rx root new,
	atom_active_window = "_NET_ACTIVE_WINDOW",
	atom_client_list = "_NET_CLIENT_LIST",
	atom_pid = "_NET_WM_PID",
	atom_desktop = "_NET_WM_DESKTOP",
	// from ICCCM, not a typo
	atom_change_state = "WM_CHANGE_STATE",
	atom_state = "_NET_WM_STATE",
	atom_state_max_vert = "_NET_WM_STATE_MAXIMIZED_VERT",
	atom_state_max_horz = "_NET_WM_STATE_MAXIMIZED_HORZ",
	atom_state_sticky = "_NET_WM_STATE_STICKY",
	atom_state_shaded = "_NET_WM_STATE_SHADED",
	atom_state_hidden = "_NET_WM_STATE_HIDDEN",
	atom_state_fullscreen = "_NET_WM_STATE_FULLSCREEN",
	atom_state_above = "_NET_WM_STATE_ABOVE",
	atom_state_below = "_NET_WM_STATE_BELOW",
);

// TODO: when adding an ipc interface do it here
fn event_thread(connection: Arc<Connection>) -> Receiver<ThreadEvent> {
	let (tx, rx) = sync_channel(0);
	let thread = move || {
		let inner_tx = tx.clone();
		_ = ctrlc::set_handler(move || {
			_ = inner_tx.send(ThreadEvent::Quit);
		});
		loop {
			let event = match connection.wait_for_event() {
				Err(err) => ThreadEvent::Err(err),
				Ok(xcb::Event::X(x::Event::PropertyNotify(evt))) => {
					ThreadEvent::PropertyNotify(evt)
				}
				Ok(xcb::Event::X(x::Event::ConfigureNotify(evt))) => {
					ThreadEvent::ConfigureNotify(evt)
				}
				Ok(xcb::Event::X(x::Event::DestroyNotify(evt))) => ThreadEvent::DestroyNotify(evt),
				Ok(_) => ThreadEvent::Other,
			};
			let Ok(()) = tx.send(event) else { break };
		}
	};
	thread::Builder::new()
		.name("Event Thread".into())
		.spawn(thread)
		.expect("Failed to start event thread");
	rx
}

impl Context {
	// TODO: this feels very swallow-specific
	pub fn next_event(&self) -> Event {
		match self.rx.recv().unwrap() {
			ThreadEvent::Quit => {
				output::quit();
				Event::Quit
			}
			ThreadEvent::Err(err) => {
				output::error(err);
				Event::Interrupted
			}
			ThreadEvent::PropertyNotify(event) => {
				if event.atom() == self.atom_client_list && event.window() == self.root {
					Event::WindowList
				} else if event.atom() == self.atom_desktop || event.atom() == self.atom_state {
					Event::Update(event.window())
				} else {
					Event::Interrupted
				}
			}
			ThreadEvent::ConfigureNotify(event) => Event::Update(event.window()),
			ThreadEvent::DestroyNotify(event) => Event::Close(event.window()),
			ThreadEvent::Other => Event::Interrupted,
		}
	}
	pub fn flush(&self) {
		_ = self.connection.flush();
	}
	fn get_property(window: Window, property: Atom, r#type: Atom, length: u32) -> x::GetProperty {
		x::GetProperty {
			delete: false,
			window,
			property,
			r#type,
			long_offset: 0,
			long_length: length,
		}
	}
	// two parts because lifetimes
	fn client_message1(window: Window, atom: Atom, data32: [u32; 5]) -> x::ClientMessageEvent {
		x::ClientMessageEvent::new(window, atom, x::ClientMessageData::Data32(data32))
	}
	fn client_message2<'event>(
		&self,
		event: &'event x::ClientMessageEvent,
	) -> x::SendEvent<'event, x::ClientMessageEvent> {
		x::SendEvent {
			propagate: false,
			destination: x::SendEventDest::Window(self.root),
			// dunno what these mean but they're copied from the C impl
			event_mask: x::EventMask::SUBSTRUCTURE_NOTIFY | x::EventMask::SUBSTRUCTURE_REDIRECT,
			event,
		}
	}
	/// like flush, but wait for the replies
	pub fn flush_hard(&self) {
		output::flush_hard();
		// dummy event to force the event queue to tick forward
		_ = self
			.connection
			.wait_for_reply(self.connection.send_request(&Self::get_property(
				self.root,
				x::ATOM_NONE,
				x::ATOM_NONE,
				0,
			)));
	}
	/// return list of desktop windows
	pub fn get_window_list(&self) -> xcb::Result<x::GetPropertyReply> {
		self.connection
			.wait_for_reply(self.connection.send_request(&Self::get_property(
				self.root,
				self.atom_client_list,
				x::ATOM_WINDOW,
				u32::MAX,
			)))
	}
	pub fn subscribe(&self, window: Window) {
		self.connection.send_request(&x::ChangeWindowAttributes {
			window,
			value_list: &[x::Cw::EventMask(
				x::EventMask::PROPERTY_CHANGE | x::EventMask::STRUCTURE_NOTIFY,
			)],
		});
	}
	pub fn get_window_geometry(&self, window: Window) -> Option<Geometry> {
		// seems weird that i get window position like this
		let position = self.connection.send_request(&x::TranslateCoordinates {
			src_window: window,
			dst_window: self.root,
			src_x: 0,
			src_y: 0,
		});
		// but GetGeometry only returns the position relative to the window frame
		let size = self.connection.send_request(&x::GetGeometry {
			drawable: x::Drawable::Window(window),
		});
		let desktop = self.connection.send_request(&Self::get_property(
			window,
			self.atom_desktop,
			x::ATOM_CARDINAL,
			4,
		));
		#[expect(
			clippy::cast_possible_truncation,
			reason = "4 gigabyte atom? in this economy?"
		)]
		let state = self.connection.send_request(&Self::get_property(
			window,
			self.atom_state,
			x::ATOM_ATOM,
			12 * size_of::<Atom>() as u32,
		));
		// all requests sent in parallel
		let position = self.connection.wait_for_reply(position).ok()?;
		let size = self.connection.wait_for_reply(size).ok()?;
		let desktop = self.connection.wait_for_reply(desktop).ok()?;
		let state = self.connection.wait_for_reply(state).ok()?;
		Some(Geometry {
			x: position.dst_x() - size.x(),
			y: position.dst_y() - size.y(),
			w: size.width(),
			h: size.height(),
			d: desktop.value().first().copied().unwrap_or_default(),
			s: WindowState::new(self, state.value::<Atom>()),
		})
	}
	pub fn set_window_geometry(&self, window: Window, geometry: Geometry) {
		output::window_move(window, geometry);
		self.connection.send_request(&x::ConfigureWindow {
			window,
			value_list: &[
				x::ConfigWindow::X(geometry.x.into()),
				x::ConfigWindow::Y(geometry.y.into()),
				x::ConfigWindow::Width(geometry.w.into()),
				x::ConfigWindow::Height(geometry.h.into()),
			],
		});
		self.connection
			.send_request(&self.client_message2(&Self::client_message1(
				window,
				self.atom_desktop,
				[geometry.d, 2, 0, 0, 0],
			)));
		for event in geometry.s.events(self) {
			self.connection
				.send_request(&self.client_message2(&Self::client_message1(
					window,
					self.atom_state,
					event,
				)));
		}
		let change_state = if geometry.s.is_hidden() { 3 } else { 1 };
		self.connection
			.send_request(&self.client_message2(&Self::client_message1(
				window,
				self.atom_change_state,
				[change_state, 0, 0, 0, 0],
			)));
	}
	pub fn show_window(&self, window: Window) {
		output::window_vis(true, window);
		self.connection.send_request(&x::MapWindow { window });
	}
	/// probably resets the window's position
	pub fn hide_window(&self, window: Window) {
		output::window_vis(false, window);
		self.connection.send_request(&x::UnmapWindow { window });
	}
	/// move the focus to a window if the focus is on a previous window
	/// (to prevent stealing the focus)
	pub fn set_window_active_if(&self, check: Window, window: Window) -> Option<Infallible> {
		let active = *self
			.connection
			.wait_for_reply(self.connection.send_request(&Self::get_property(
				self.root,
				self.atom_active_window,
				x::ATOM_WINDOW,
				4,
			)))
			.ok()?
			.value::<Window>()
			.first()?;
		if active == check {
			output::window_refocus(check, window);
			self.connection
				.send_request(&self.client_message2(&Self::client_message1(
					window,
					self.atom_active_window,
					[2, 0, 0, 0, 0],
				)));
		}
		None
	}
	fn window_pid_request(&self, window: Window) -> x::GetPropertyCookie {
		self.connection.send_request(&Self::get_property(
			window,
			self.atom_pid,
			x::ATOM_CARDINAL,
			4,
		))
	}
	fn window_pid_reply(&self, cookie: x::GetPropertyCookie) -> Option<u32> {
		self.connection
			.wait_for_reply(cookie)
			.ok()
			.and_then(|reply| reply.value::<u32>().first().copied())
	}
	pub fn window_pid(&self, window: Window) -> Option<u32> {
		self.window_pid_reply(self.window_pid_request(window))
	}

	/// fallback checks probably too many windows,
	/// but it's a rare* enough case that it probably won't hurt
	// TODO: maybe this is entirely unneeded?
	// how can the parent open while the child is processing?
	fn fallback_iter(&self) -> impl Iterator<Item = Window> + '_ {
		let mut queue = vec![self.root];
		let mut requests = Vec::<x::QueryTreeCookie>::new();
		iter::from_fn(move || {
			if queue.is_empty() {
				let reply = self.connection.wait_for_reply(requests.pop()?);
				queue.extend_from_slice(reply.ok()?.children());
			}
			while let Some(reply) = requests
				.last()
				.and_then(|request| self.connection.poll_for_reply(request))
			{
				requests.pop();
				queue.extend_from_slice(reply.ok()?.children());
			}
			let window = queue.pop()?;
			requests.push(self.connection.send_request(&x::QueryTree { window }));
			Some(window)
		})
	}
	/// this is only used for terminals so it should be fine
	/// to assume one window per pid, not sure how swallowing
	/// a multi-window application would work anyways
	pub fn find_window_with_pid(&self, pid: u32, window_list: &[Window]) -> Option<Window> {
		const PARALLEL_REQUESTS: usize = 10;
		let mut queue = VecDeque::with_capacity(PARALLEL_REQUESTS);
		let mut requests = window_list
			.iter()
			.copied()
			.chain(self.fallback_iter())
			.map(|window| (window, self.window_pid_request(window)));
		// start multiple requests in parallel
		iter::from_fn(move || {
			queue.extend(requests.by_ref().take(PARALLEL_REQUESTS - queue.len()));
			queue.pop_back()
		})
		.filter_map(|(window, request)| self.window_pid_reply(request).map(|res| (window, res)))
		.find(|&reply| reply.1 == pid)
		.map(|reply| reply.0)
	}
}

// [porting] use whatever sane-ish api exists, it's probably better than this
/// programmer rolls “worst parser ever”, asked to leave /proc/{pid}/status
pub fn get_pid_info(pid: u32) -> Option<(u32, Vec<u8>)> {
	let mut file = BufReader::new(File::open(format!("/proc/{pid}/status")).ok()?);
	let (mut ppid, mut name, mut line) = (None, None, Vec::new());
	while ppid.is_none() || name.is_none() {
		line.clear();
		file.read_until(b'\n', &mut line).ok()?;
		line.pop();
		if let Some(ppid_text) = line.strip_prefix(b"PPid:\t") {
			ppid = Some(from_utf8(ppid_text).ok()?.parse().ok()?);
		} else if let Some(name_text) = line.strip_prefix(b"Name:\t") {
			#[cfg_attr(rust_analyzer, expect(unused_mut, reason = "rust-analyzer#18209"))]
			let mut normal = true;
			let iter = name_text.iter().filter_map(|&(mut ch)| {
				(normal, ch) = match ch {
					b'\\' if normal => (false, ch),
					b'n' if !normal => (true, b'\n'),
					_ => (true, ch),
				};
				normal.then_some(ch)
			});
			name = Some(iter.collect());
		}
	}
	Some((ppid.unwrap(), name.unwrap()))
}
