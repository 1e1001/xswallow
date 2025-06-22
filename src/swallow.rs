//! Window swallowing
use std::cell::Cell;
use std::mem::take;
use std::rc::{Rc, Weak};
use std::{fmt, io};

use foldhash::fast::RandomState;
use foldhash::{HashMap, HashSet};
use iter_debug::DebugIterator;
use multiline_logger::log;
use weak_table::WeakValueHashMap;
use weak_table::weak_value_hash_map::Entry as WvhmEntry;
use xcb::XidNew;
use xcb::x::{self, Window};

use crate::context::{Builder, Component, Context, Geometry, Pid, RawEvent};
use crate::control::CtrlEvent;
use crate::{atom, config};

/// xprop-like window picker ui
pub fn window_picker() -> u32 {
	todo!("window picker");
}

#[derive(Default)]
pub struct Config {
	pub terminal: HashSet<Rc<[u8]>>,
	pub immune: HashSet<Rc<[u8]>>,
}

impl fmt::Debug for Config {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		// not worth making a custom implementation for something that only gets called
		// once
		f.debug_struct("Config")
			.field(
				"terminal",
				&self
					.terminal
					.iter()
					.map(|name| String::from_utf8_lossy(name))
					.debug(),
			)
			.field(
				"immune",
				&self
					.immune
					.iter()
					.map(|name| String::from_utf8_lossy(name))
					.debug(),
			)
			.finish()
	}
}

impl config::Config for Config {
	fn property(&mut self, line: &[u8]) -> Result<(), config::ParseError> {
		if let Some(terminal) = config::key_then(line, b"terminal:") {
			self.terminal
				.extend(config::value_text_list(terminal?).map(Into::into));
		} else if let Some(immune) = config::key_then(line, b"immune:") {
			self.immune
				.extend(config::value_text_list(immune?).map(Into::into));
		} else {
			return Err(config::ParseError::InvalidPropertyName);
		}
		Ok(())
	}
}

/// returns an iterator of all the new values
fn list_diff<'data>(
	old: &'data mut Vec<Window>,
	new: &'data [Window],
) -> impl Iterator<Item = Window> + use<'data> {
	// since the lists are mostly the same, start reading into the list at an
	// "expected" index equivalent to one after the index of the previous result
	let mut start_index = 0;
	new.iter().copied().filter(move |&id| {
		for index in (start_index..old.len()).chain(0..start_index) {
			if old[index] == id {
				old.swap_remove(index);
				start_index = (index + 1).min(old.len());
				return false;
			}
		}
		// index wasn't found so it's new
		true
	})
}

struct Parent {
	window: Window,
	/// fallback position when force-showing (on quit or ctrl)
	position: Geometry,
	show: Cell<bool>,
}

struct Child {
	settled: bool,
	parent: Rc<Parent>,
	position: Geometry,
}
pub struct State {
	config: Config,
	all_windows: Vec<Window>,
	parent_by_pid: WeakValueHashMap<Pid, Weak<Parent>, RandomState>,
	parent_by_window: WeakValueHashMap<Window, Weak<Parent>, RandomState>,
	child_by_window: HashMap<Window, Child>,
}

#[expect(clippy::unnecessary_wraps, reason = "consistency")]
impl State {
	fn quit(&mut self, cx: &Context) -> xcb::Result<()> {
		// show all the windows that were hidden
		for parent in self.parent_by_pid.values() {
			// move twice to make sure it's in the correct position
			// i've had bugs with doing either one so i'm scared to remove it
			cx.move_window(parent.window, parent.position);
			cx.show_window(parent.window, true);
			cx.move_window(parent.window, parent.position);
		}
		// make sure requests actually get received
		cx.flush(true);
		Ok(())
	}
	// TODO: don't take option pid?
	fn find_parent(&self, mut parent_pid: Option<Pid>) -> io::Result<Option<(Vec<u8>, Pid)>> {
		while let Some(pid) = parent_pid {
			let (name, next_ppid) = pid.info()?;
			if self.config.terminal.contains(&*name) {
				return Ok(Some((name, pid)));
			} else if self.config.immune.contains(&*name) {
				return Ok(None);
			}
			parent_pid = next_ppid;
		}
		Ok(None)
	}
	fn open(
		&mut self,
		cx: &Context,
		child_window: Window,
		all_windows: Option<&[Window]>,
		late: bool,
	) -> io::Result<()> {
		let all_windows = all_windows.unwrap_or(&self.all_windows);
		let Some(child_pid) = cx.window_pid(child_window).map_err(io::Error::other)? else {
			// can't find pid
			return Ok(());
		};
		let (child_name, parent_pid) = child_pid.info()?;
		if self.config.immune.contains(&*child_name) {
			// child immune
			return Ok(());
		}
		let Some((_parent_name, parent_pid)) = self.find_parent(parent_pid)? else {
			// parent immune / missing
			return Ok(());
		};
		let Some(parent_window) = cx.find_window_with_pid(parent_pid, all_windows) else {
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"parent process has no window",
			));
		};
		// parent reference
		let parent;
		// stored position
		let position;
		match self.parent_by_pid.entry(parent_pid) {
			WvhmEntry::Occupied(entry) => {
				// just use existing position
				position = cx.window_geometry(child_window).map_err(io::Error::other)?;
				parent = entry.get_strong();
			}
			WvhmEntry::Vacant(entry) => {
				// move child and create entry
				position = cx
					.window_geometry(if late { child_window } else { parent_window })
					.map_err(io::Error::other)?;
				parent = entry.insert(Rc::new(Parent {
					window: parent_window,
					position,
					show: Cell::new(false),
				}));
				cx.show_window(parent_window, false);
				if !late {
					cx.move_window(child_window, position);
				}
			}
		}
		cx.subscribe(
			child_window,
			x::EventMask::PROPERTY_CHANGE | x::EventMask::STRUCTURE_NOTIFY,
		);
		cx.flush(false);
		self.parent_by_window
			.insert(parent_window, Rc::clone(&parent));
		self.child_by_window.insert(child_window, Child {
			settled: false,
			parent,
			position,
		});
		Ok(())
	}
	fn update(&mut self, cx: &Context, window: Window) -> xcb::Result<()> {
		if let Some(child) = self.child_by_window.get_mut(&window) {
			let position = cx.window_geometry(window)?;
			if child.settled {
				child.position = position;
			} else {
				if position != child.position {
					child.position = child.position.settle(position);
					cx.move_window(window, child.position);
				}
				child.settled = true;
			}
		}
		Ok(())
	}
	fn close(&mut self, cx: &Context, window: Window) -> xcb::Result<()> {
		if let Some(Child {
			parent, position, ..
		}) = self.child_by_window.remove(&window)
		{
			// no more child windows open
			if Rc::strong_count(&parent) == 1 {
				// specific order to prevent “not working”
				cx.show_window(parent.window, true);
				cx.move_window(parent.window, position);
				cx.move_focus(window, parent.window)?;
				// not sure if i need this
				cx.flush(false);
			}
		}
		Ok(())
	}
	fn refresh(&mut self, cx: &Context) -> xcb::Result<()> {
		let new_windows = cx.get_window_list()?;
		let new_windows = new_windows.value::<Window>();
		// take here to prevent double-mutable-reference
		let mut all_windows = take(&mut self.all_windows);
		for child_window in list_diff(&mut all_windows, new_windows) {
			// keep scanning the next windows
			if let Err(err) = self.open(cx, child_window, Some(new_windows), false) {
				log::error!("New window failed: {err}\n{err:?}");
			}
		}
		// replace the list with the new one
		self.all_windows = all_windows;
		self.all_windows.clear();
		self.all_windows.extend_from_slice(new_windows);
		Ok(())
	}
	fn toggle(&mut self, cx: &Context, window: Window) -> xcb::Result<()> {
		// try to do something with a user-given window id
		// if it's a child window, use its parent instead
		log::debug!("Toggle {window:?}");
		let parent = if let Some(child) = self.child_by_window.get(&window) {
			Rc::clone(&child.parent)
		} else if let Some(parent) = self.parent_by_window.get(&window) {
			parent
		} else {
			_ = self.open(cx, window, None, true);
			return Ok(());
		};
		log::trace!("parent = {:?}", parent.window);
		// - hide / unhide the parent window, but keep child data around
		let show = !parent.show.get();
		parent.show.set(show);
		log::trace!("show = {show:?}");
		// TODO: this drags focus when showing?
		cx.show_window(parent.window, show);
		if show {
			// this probably won't do anything but it's nice to have
			cx.move_window(parent.window, parent.position);
		}
		cx.flush(false);
		Ok(())
	}
}

impl Component for State {
	type Config = Config;
	const NAME: &str = "swallow";
	fn pre_init(_cx: &mut Builder, mut config: Self::Config) -> Self {
		// add terminals to immune (so you don't swallow a terminal!)
		config.immune.extend(config.terminal.iter().cloned());
		Self {
			config,
			all_windows: Vec::new(),
			parent_by_pid: WeakValueHashMap::default(),
			parent_by_window: WeakValueHashMap::default(),
			child_by_window: HashMap::default(),
		}
	}
	fn init(&mut self, cx: &Context) -> xcb::Result<()> {
		cx.subscribe(cx.root, x::EventMask::PROPERTY_CHANGE);
		let all_windows = cx.get_window_list()?.value().to_vec();
		for &window in &all_windows {
			_ = self.open(cx, window, Some(&all_windows), true);
		}
		self.all_windows = all_windows;
		Ok(())
	}
	fn event(&mut self, cx: &Context, event: &RawEvent) -> xcb::Result<()> {
		match event {
			RawEvent::Quit => self.quit(cx),
			RawEvent::Control(CtrlEvent::SwallowToggle(window)) => {
				self.toggle(
					cx,
					// SAFETY: https://github.com/rust-x-bindings/rust-xcb/issues/197
					unsafe { Window::new(*window) },
				)
			}
			RawEvent::X(xcb::Event::X(x::Event::PropertyNotify(event)))
				if event.atom() == *atom::_NET_CLIENT_LIST && event.window() == cx.root =>
			{
				self.refresh(cx)
			}
			RawEvent::X(xcb::Event::X(x::Event::PropertyNotify(event)))
				if event.atom() == *atom::_NET_WM_DESKTOP
					|| event.atom() == *atom::_NET_WM_STATE =>
			{
				self.update(cx, event.window())
			}
			RawEvent::X(xcb::Event::X(x::Event::ConfigureNotify(event))) => {
				self.update(cx, event.window())
			}
			RawEvent::X(xcb::Event::X(x::Event::DestroyNotify(event))) => {
				self.close(cx, event.window())
			}
			_ => Ok(()),
		}
	}
}
