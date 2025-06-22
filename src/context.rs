//! interaction between components

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufReader};
use std::num::NonZeroU32;
use std::ops::BitOr;
use std::path::PathBuf;
use std::sync::Arc;
use std::{array, fmt, iter, thread};

use foldhash::{HashMap, HashSet};
use multiline_logger::log;
use tokio::signal::ctrl_c;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::task::spawn;
use xcb::x::{self, Atom, Window};
use xcb::{Connection, Xid, xkb};

use crate::atom;
use crate::control::{self, CtrlEvent};

#[derive(Debug)]
pub enum RawEvent {
	Quit,
	Control(CtrlEvent),
	X(xcb::Event),
}

// mirror of xcb::Extension with nice features
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Extension {
	Xkb,
}
impl Extension {
	fn from_xcb(ext: xcb::Extension) -> Option<Self> {
		Some(match ext {
			xcb::Extension::Xkb => Self::Xkb,
			_ => return None,
		})
	}
	fn xcb_name(self) -> xcb::Extension {
		match self {
			Extension::Xkb => xcb::Extension::Xkb,
		}
	}
	fn verify(self, connection: &Connection) -> bool {
		match self {
			Self::Xkb =>
			{
				#[expect(clippy::cast_possible_truncation, reason = "x protocol sucks")]
				connection
					.wait_for_reply(connection.send_request(&xkb::UseExtension {
						wanted_major: xkb::MAJOR_VERSION as u16,
						wanted_minor: xkb::MINOR_VERSION as u16,
					}))
					.is_ok_and(|v| v.supported())
			}
		}
	}
}

pub struct Builder {
	current_component: usize,
	extensions: HashMap<Extension, Vec<usize>>,
}

impl Builder {
	pub fn new() -> Self {
		Self {
			current_component: 0,
			extensions: HashMap::default(),
		}
	}
	pub fn pre_init<C: Component>(&mut self, config: Option<C::Config>) -> Option<C> {
		config.map(|config| {
			log::trace!("Pre-init {:?}", C::NAME);
			let component = C::pre_init(self, config);
			self.current_component += 1;
			component
		})
	}
	pub fn require_extension(&mut self, ext: Extension) {
		self.extensions
			.entry(ext)
			.or_default()
			.push(self.current_component);
	}
	pub fn build(mut self, control: PathBuf) -> Context {
		let (connection, screen) = Connection::connect_with_extensions(
			None,
			&[],
			self.extensions
				.keys()
				.map(|v| v.xcb_name())
				.collect::<Vec<_>>()
				.as_slice(),
		)
		.expect("failed to connect to X");
		let connection = Arc::new(connection);
		let root = connection
			.get_setup()
			.roots()
			.nth(usize::try_from(screen).unwrap_or_default())
			.expect("No screen")
			.root();
		atom::init(&connection);
		let connection = Arc::clone(&connection);
		// unfortunate buffer capacity here, i'd love to make it 0
		let (tx, rx) = channel(1);
		let event_tx = tx.clone();
		let inner_connection = Arc::clone(&connection);
		// xcb events thread
		// not a task because xcb is blocking
		thread::Builder::new()
			.name("xcb events".into())
			.spawn(move || {
				loop {
					let event = match inner_connection.wait_for_event() {
						Err(err) => {
							log::error!("Error: {err}\n{err:?}");
							continue;
						}
						Ok(event) => RawEvent::X(event),
					};
					let Ok(()) = event_tx.blocking_send(event) else {
						break;
					};
				}
			})
			.expect("Failed to start event thread");
		let mut skip = HashSet::default();
		for ext in connection
			.active_extensions()
			.filter_map(Extension::from_xcb)
		{
			log::trace!("Loading Extension {ext:?}");
			if ext.verify(&connection) {
				self.extensions.remove(&ext);
			} else {
				log::warn!("Extension {ext:?} failed");
			}
		}
		skip.extend(self.extensions.into_values().flatten());
		// clean exit task
		let ctrlc_tx = tx.clone();
		spawn(async move {
			ctrl_c().await.expect("^C handler failed");
			_ = ctrlc_tx.send(RawEvent::Quit).await;
		});
		if !control.as_os_str().is_empty() {
			// control port task
			let ctrl_tx = tx.clone();
			spawn(control::run_control_loop(control, move |event| {
				let inner_tx = ctrl_tx.clone();
				async move {
					inner_tx.send(RawEvent::Control(event)).await.unwrap();
				}
			}));
		}
		//output::setup_context(screen, root);
		Context {
			connection,
			rx,
			tx,
			root,
			current_component: 0,
			skip,
		}
	}
}

pub struct Context {
	pub connection: Arc<Connection>,
	tx: Sender<RawEvent>,
	rx: Receiver<RawEvent>,
	pub root: x::Window,
	current_component: usize,
	skip: HashSet<usize>,
}

impl Context {
	pub fn init<C: Component>(&mut self, component: Option<C>) -> Option<C> {
		component.and_then(|mut component| {
			log::trace!("Init {:?}", C::NAME);
			let idx = self.current_component;
			self.current_component += 1;
			if self.skip.contains(&idx) {
				log::warn!("Skipping component {:?} due to missing extension", C::NAME);
				return None;
			}
			match component.init(self) {
				Ok(()) => Some(component),
				Err(err) => {
					log::error!(
						"Component {:?}\nfailed to initialize\nwith error {err}\n{err:?}",
						C::NAME,
					);
					None
				}
			}
		})
	}
	pub async fn next_event(&mut self) -> RawEvent {
		self.rx.recv().await.expect("event tx closed")
	}
	pub fn event<C: Component>(&self, event: &RawEvent, component: &mut Option<C>) {
		if let Some(component) = component {
			if let Err(err) = component.event(self, event) {
				log::error!(
					"Component {:?}\nfailed to handle event {event:?}\nwith error {err}\n{err:?}",
					C::NAME,
				);
			}
		}
	}
	/// struct shorthands
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
	fn client1(window: Window, atom: Atom, data32: [u32; 5]) -> x::ClientMessageEvent {
		x::ClientMessageEvent::new(window, atom, x::ClientMessageData::Data32(data32))
	}
	fn client2<'event>(
		&self,
		client1: &'event x::ClientMessageEvent,
	) -> x::SendEvent<'event, x::ClientMessageEvent> {
		x::SendEvent {
			propagate: false,
			destination: x::SendEventDest::Window(self.root),
			// dunno what these mean but they're copied from the C impl
			event_mask: x::EventMask::SUBSTRUCTURE_NOTIFY | x::EventMask::SUBSTRUCTURE_REDIRECT,
			event: client1,
		}
	}
	// pid shorthands
	fn pid1(&self, window: Window) -> x::GetPropertyCookie {
		self.connection.send_request(&Self::get_property(
			window,
			*atom::_NET_WM_PID,
			x::ATOM_CARDINAL,
			4,
		))
	}
	fn pid2(&self, cookie: x::GetPropertyCookie) -> xcb::Result<Option<Pid>> {
		self.connection.wait_for_reply(cookie).map(|reply| {
			reply
				.value::<u32>()
				.first()
				.copied()
				.and_then(NonZeroU32::new)
				.map(Pid)
		})
	}
	/// fallback checks probably too many windows,
	/// but it's a rare enough case that it probably won't hurt
	fn fallback_iter(&self) -> impl Iterator<Item = Window> + use<'_> {
		let mut queue = vec![self.root];
		let mut requests = Vec::<x::QueryTreeCookie>::new();
		let mut once = true;
		iter::from_fn(move || {
			if once {
				log::warn!("Using fallback iterator… this might be slow");
				once = false;
			}
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

	// and here's all the x-connection functionality

	/// get a new tx for writing events to
	pub fn new_tx(&self) -> Sender<RawEvent> {
		self.tx.clone()
	}
	/// ensures all messages have been sent to the x server, `hard` ensures that
	/// all messages have also been received
	pub fn flush(&self, hard: bool) {
		if hard {
			_ = self
				.connection
				.wait_for_reply(self.connection.send_request(&Self::get_property(
					self.root,
					x::ATOM_NONE,
					x::ATOM_NONE,
					0,
				)));
		} else {
			_ = self.connection.flush();
		}
	}
	/// returns a reply with the list of open window id's
	pub fn get_window_list(&self) -> xcb::Result<x::GetPropertyReply> {
		self.connection
			.wait_for_reply(self.connection.send_request(&Self::get_property(
				self.root,
				*atom::_NET_CLIENT_LIST,
				x::ATOM_WINDOW,
				// i think this is wrong but it Works™
				u32::MAX,
			)))
	}
	/// Sets the visibility status of a window
	/// not to be confused with `_NET_WM_STATE_HIDDEN`
	pub fn show_window(&self, window: Window, visible: bool) {
		// no dyn :(
		if visible {
			self.connection.send_request(&x::MapWindow { window });
		} else {
			self.connection.send_request(&x::UnmapWindow { window });
		}
	}
	/// Move window focus from one window to another
	pub fn move_focus(&self, from: Window, to: Window) -> xcb::Result<()> {
		match self
			.connection
			.wait_for_reply(self.connection.send_request(&Self::get_property(
				self.root,
				*atom::_NET_ACTIVE_WINDOW,
				x::ATOM_WINDOW,
				4,
			)))?
			.value::<Window>()
			.first()
		{
			Some(&active) if active == from => {
				self.connection.send_request(&self.client2(&Self::client1(
					to,
					*atom::_NET_ACTIVE_WINDOW,
					[2, 0, 0, 0, 0],
				)));
			}
			_ => {}
		}
		Ok(())
	}
	/// Sets the geometry of the window
	pub fn move_window(&self, window: Window, geometry: Geometry) {
		let prop_x = x::ConfigWindow::X(geometry.x.into());
		let prop_y = x::ConfigWindow::Y(geometry.y.into());
		let prop_w = x::ConfigWindow::Width(geometry.w.into());
		let prop_h = x::ConfigWindow::Height(geometry.h.into());
		let value_list = match (geometry.s.is_max_vert(), geometry.s.is_max_horz()) {
			(false, false) => &[prop_x, prop_y, prop_w, prop_h][..],
			(false, true) => &[prop_y, prop_h],
			(true, false) => &[prop_x, prop_w],
			(true, true) => &[],
		};
		// apply position
		self.connection
			.send_request(&x::ConfigureWindow { window, value_list });
		// apply desktop
		self.connection.send_request(&self.client2(&Self::client1(
			window,
			*atom::_NET_WM_DESKTOP,
			[geometry.d, 2, 0, 0, 0],
		)));
		// apply most states
		for event in geometry.s.events() {
			self.connection.send_request(&self.client2(&Self::client1(
				window,
				*atom::_NET_WM_STATE,
				event,
			)));
		}
		// apply hidden state
		let change_state = if geometry.s.is_hidden() { 3 } else { 1 };
		self.connection.send_request(&self.client2(&Self::client1(
			window,
			*atom::WM_CHANGE_STATE,
			[change_state, 0, 0, 0, 0],
		)));
	}
	/// Gets the geometry of a window
	pub fn window_geometry(&self, window: Window) -> xcb::Result<Geometry> {
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
			*atom::_NET_WM_DESKTOP,
			x::ATOM_CARDINAL,
			4,
		));
		#[expect(
			clippy::cast_possible_truncation,
			reason = "4 gigabyte atom? in this economy?"
		)]
		let state = self.connection.send_request(&Self::get_property(
			window,
			*atom::_NET_WM_STATE,
			x::ATOM_ATOM,
			12 * size_of::<Atom>() as u32,
		));
		// wait for responses after sending all requests
		let position = self.connection.wait_for_reply(position)?;
		let size = self.connection.wait_for_reply(size)?;
		let desktop = self.connection.wait_for_reply(desktop)?;
		let state = self.connection.wait_for_reply(state)?;
		Ok(Geometry {
			x: position.dst_x() - size.x(),
			y: position.dst_y() - size.y(),
			w: size.width(),
			h: size.height(),
			// defaulting to desktop 0 seems weird but i guess it'll be fine.
			// worst case a window is in the wrong desktop, but it should always be visible
			d: desktop.value().first().copied().unwrap_or_default(),
			s: WindowState::new(state.value::<Atom>()),
		})
	}
	pub fn subscribe(&self, window: Window, mask: x::EventMask) {
		self.connection.send_request(&x::ChangeWindowAttributes {
			window,
			value_list: &[x::Cw::EventMask(mask)],
		});
	}
	/// Get the process id associated with a window
	pub fn window_pid(&self, window: Window) -> xcb::Result<Option<Pid>> {
		self.pid2(self.pid1(window))
	}
	/// this is only used for terminals so it should be fine
	/// to assume one window per pid, not sure how swallowing
	/// a multi-window application would work anyways
	pub fn find_window_with_pid(&self, pid: Pid, base_list: &[Window]) -> Option<Window> {
		// does this number mean anything? no
		const PARALLEL_REQUESTS: usize = 17;
		let mut queue = VecDeque::with_capacity(PARALLEL_REQUESTS);
		let mut requests = base_list
			.iter()
			.copied()
			.chain(self.fallback_iter())
			.map(|window| (window, self.pid1(window)));
		// start multiple requests in parallel
		iter::from_fn(move || {
			queue.extend(requests.by_ref().take(PARALLEL_REQUESTS - queue.len()));
			queue.pop_back()
		})
		.find_map(|(window, request)| match self.pid2(request) {
			Ok(Some(found)) if found == pid => Some(window),
			_ => None,
		})
	}
	/// Enable or disable the system bell
	/// Requires the `xkb` extension
	pub fn set_bell(&self, enable: bool) -> xcb::Result<()> {
		// since for some reason xcb doesn't have changeenabledcontrols
		// i need to do it manually
		let controls = self
			.connection
			.wait_for_reply(self.connection.send_request(&xkb::GetControls {
				device_spec: xkb::Id::UseCoreKbd as xkb::DeviceSpec,
			}))?;
		// anything that has an "affect" option i'll still leave empty though
		self.connection.send_request(&xkb::SetControls {
			device_spec: xkb::Id::UseCoreKbd as xkb::DeviceSpec,
			affect_internal_real_mods: x::ModMask::empty(),
			internal_real_mods: x::ModMask::empty(),
			affect_ignore_lock_real_mods: x::ModMask::empty(),
			ignore_lock_real_mods: x::ModMask::empty(),
			affect_internal_virtual_mods: xkb::VMod::empty(),
			internal_virtual_mods: xkb::VMod::empty(),
			affect_ignore_lock_virtual_mods: xkb::VMod::empty(),
			ignore_lock_virtual_mods: xkb::VMod::empty(),
			mouse_keys_dflt_btn: controls.mouse_keys_dflt_btn(),
			groups_wrap: controls.groups_wrap(),
			access_x_options: controls.access_x_option(),
			// and here's the two i actually change!
			affect_enabled_controls: xkb::BoolCtrl::AUDIBLE_BELL_MASK,
			enabled_controls: if enable {
				xkb::BoolCtrl::AUDIBLE_BELL_MASK
			} else {
				xkb::BoolCtrl::empty()
			},
			change_controls: xkb::Control::empty(),
			repeat_delay: controls.repeat_delay(),
			repeat_interval: controls.repeat_interval(),
			slow_keys_delay: controls.slow_keys_delay(),
			debounce_delay: controls.debounce_delay(),
			mouse_keys_delay: controls.mouse_keys_delay(),
			mouse_keys_interval: controls.mouse_keys_interval(),
			mouse_keys_time_to_max: controls.mouse_keys_time_to_max(),
			mouse_keys_max_speed: controls.mouse_keys_max_speed(),
			mouse_keys_curve: controls.mouse_keys_curve(),
			access_x_timeout: controls.access_x_timeout(),
			access_x_timeout_mask: controls.access_x_timeout_mask(),
			access_x_timeout_values: controls.access_x_timeout_values(),
			access_x_timeout_options_mask: controls.access_x_timeout_options_mask(),
			access_x_timeout_options_values: controls.access_x_timeout_options_values(),
			per_key_repeat: *controls.per_key_repeat(),
		});
		Ok(())
	}
}

pub trait Component {
	type Config;
	const NAME: &str;
	fn pre_init(cx: &mut Builder, config: Self::Config) -> Self;
	fn init(&mut self, cx: &Context) -> xcb::Result<()>;
	fn event(&mut self, cx: &Context, event: &RawEvent) -> xcb::Result<()>;
}

/// newtype for process ids
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pid(NonZeroU32);

impl Pid {
	/// Get some info from the process's info file
	/// Currently returns the name and the parent's Pid
	pub fn info(self) -> io::Result<(Vec<u8>, Option<Pid>)> {
		use crate::config::{Config, ParseError, key_then, parse_file, value_integer, value_text};
		#[derive(Default)]
		struct Status {
			name: Option<Vec<u8>>,
			#[expect(clippy::option_option, reason = "actually meaningful")]
			ppid: Option<Option<Pid>>,
		}
		impl Config for Status {
			fn property(&mut self, line: &[u8]) -> Result<(), ParseError> {
				if let Some(name) = key_then(line, b"Name:") {
					self.name = Some(value_text(name?));
				} else if let Some(ppid) = key_then(line, b"PPid:") {
					self.ppid = Some(NonZeroU32::new(value_integer(ppid?)?).map(Pid));
				}
				// stop processing once all config is acquired
				if self.name.is_some() && self.ppid.is_some() {
					Err(ParseError::EarlyExit)
				} else {
					Ok(())
				}
			}
		}
		let file = BufReader::new(File::open(format!("/proc/{}/status", self.0))?);
		let status = parse_file::<Status, _>(file).map_err(io::Error::other)?;
		let Status {
			name: Some(name),
			ppid: Some(ppid),
		} = status
		else {
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"pid status file missing info",
			));
		};
		Ok((name, ppid))
	}
}

/// where a window is
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
	// these are 16 because that's what i can *read*, even if writing uses 32
	x: i16,
	y: i16,
	w: u16,
	h: u16,
	/// desktop number
	d: u32,
	s: WindowState,
}

impl Geometry {
	/// Calculate "settled" window position based on expected and measured
	/// values
	pub fn settle(self, measured: Self) -> Self {
		Self {
			// offset by amount of error
			x: self.x.wrapping_add(self.x).wrapping_sub(measured.x),
			y: self.y.wrapping_add(self.y).wrapping_sub(measured.y),
			..self
		}
	}
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

/// `_NET_WM_STATE` in a bitfield
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct WindowState(u8);
impl WindowState {
	fn new(list: &[Atom]) -> Self {
		let max_vert = *atom::_NET_WM_STATE_MAXIMIZED_VERT;
		let max_horz = *atom::_NET_WM_STATE_MAXIMIZED_HORZ;
		let sticky = *atom::_NET_WM_STATE_STICKY;
		let shaded = *atom::_NET_WM_STATE_SHADED;
		let hidden = *atom::_NET_WM_STATE_HIDDEN;
		let fullscreen = *atom::_NET_WM_STATE_FULLSCREEN;
		let above = *atom::_NET_WM_STATE_ABOVE;
		let below = *atom::_NET_WM_STATE_BELOW;
		// TODO: there's a better way to do this match
		Self(
			list.iter()
				.map(|&i| match () {
					() if i == max_vert => 0x01,
					() if i == max_horz => 0x02,
					() if i == sticky => 0x04,
					() if i == shaded => 0x08,
					() if i == hidden => 0x10,
					() if i == fullscreen => 0x20,
					() if i == above => 0x40,
					() if i == below => 0x80,
					() => 0,
				})
				.reduce(BitOr::bitor)
				.unwrap_or_default(),
		)
	}
	fn take_one(&mut self) -> usize {
		let index = self.0.trailing_zeros();
		self.0 &= !1_u8.wrapping_shl(index);
		index as usize
	}
	/// list of state events needed to make a window enter this state.
	/// does not update the minimized state
	fn events(self) -> [[u32; 5]; 4] {
		// why 4 messages?
		// - we have 7 flags that need to be changed, always encode all of them.
		// - each message can contain 1 or 2 properties, both set to the same value
		// :  [0]    [1]     [2]     [3]
		// 1:  on/    off/off off/off off/off
		// 2:  on/on  off/off off/off off/
		// 3:  on/on   on/    off/off off/off
		// 4:  on/on   on/on  off/off off/
		// 5:  on/on   on/on   on/    off/off
		// 6:  on/on   on/on   on/on  off/
		// 7:  on/on   on/on   on/on   on/
		let atoms = [
			atom::_NET_WM_STATE_MAXIMIZED_VERT.resource_id(),
			atom::_NET_WM_STATE_MAXIMIZED_HORZ.resource_id(),
			atom::_NET_WM_STATE_STICKY.resource_id(),
			atom::_NET_WM_STATE_SHADED.resource_id(),
			atom::_NET_WM_STATE_HIDDEN.resource_id(),
			atom::_NET_WM_STATE_FULLSCREEN.resource_id(),
			atom::_NET_WM_STATE_ABOVE.resource_id(),
			atom::_NET_WM_STATE_BELOW.resource_id(),
			x::ATOM_NONE.resource_id(),
		];
		let mut next = Self(self.0 & !0x10);
		let mut state = 1;
		// TODO: rewrite this to not look awful
		array::from_fn(|_| {
			if next.0 == 0 {
				next = Self(!self.0 & !0x10);
				state = 0;
			}
			[state, atoms[next.take_one()], atoms[next.take_one()], 2, 0]
		})
	}
	/// iterator of short property names
	fn names(mut self) -> impl Iterator<Item = &'static str> {
		const ATOM_NAMES: &str = "+V+H+S-S-M+M+O-O";
		iter::from_fn(move || {
			let atom = self.take_one() * 2;
			ATOM_NAMES.get(atom..atom + 2)
		})
	}
	fn is_max_vert(self) -> bool {
		self.0 & 0x01 != 0
	}
	fn is_max_horz(self) -> bool {
		self.0 & 0x02 != 0
	}
	fn is_hidden(self) -> bool {
		self.0 & 0x10 != 0
	}
}
