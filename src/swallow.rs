//! core application logic
use std::convert::Infallible;
use std::env::var_os;
use std::os::unix::ffi::OsStringExt;
use std::rc::{Rc, Weak};

use foldhash::fast::RandomState;
use foldhash::{HashMap, HashSet};
use weak_table::WeakValueHashMap;
use weak_table::weak_value_hash_map::Entry as WvhmEntry;
use xcb::x::Window;

use crate::context::{Context, Geometry, get_pid_info};
use crate::output;

struct Parent {
	window: Window,
	/// position not updated with children, for use when quitting
	position: Geometry,
}

struct Child {
	pid: u32,
	parent: Rc<Parent>,
	position: Geometry,
}

/// optimized to work for lists where the prefix is the same
/// as is the case with `_NET_WM_CLIENT_LIST`
fn list_diff<T: Eq + Copy, F: FnMut(T) -> R, R>(target: &mut Vec<T>, source: &[T], mut call: F) {
	for (i, val) in source.iter().enumerate() {
		let j = target
			.iter()
			.enumerate()
			.skip(i)
			.find_map(|(j, test)| (test == val).then_some(j))
			.unwrap_or_else(|| {
				call(*val);
				target.push(*val);
				target.len() - 1
			});
		target.swap(i, j);
	}
	target.truncate(source.len());
}

/// main pid-walking algorithm
/// can't be on `State` because it's borrowed by `list_diff`
fn find_parent(
	mut parent_pid: u32,
	immune_names: &HashSet<Rc<[u8]>>,
	terminals_names: &HashSet<Rc<[u8]>>,
) -> Option<(u32, Vec<u8>)> {
	while parent_pid > 0 {
		let (next_ppid, parent_name) = get_pid_info(parent_pid)?;
		output::find_next_parent(parent_pid, &parent_name);
		if terminals_names.contains(parent_name.as_slice()) {
			return Some((parent_pid, parent_name));
		} else if immune_names.contains(parent_name.as_slice()) {
			return None;
		}
		parent_pid = next_ppid;
	}
	None
}

// TODO: replace with a real configuration file
fn env_bytes(name: &str) -> Vec<u8> {
	// since reading the /proc/{pid}/status returns arbitrary bytes,
	// i need to support more than utf-8, so i use the unix strings
	// [porting] figure out how your platform does process names
	var_os(name).unwrap_or_default().into_vec()
}
fn env_iter(text: &[u8]) -> impl Iterator<Item = Rc<[u8]>> + '_ {
	let iter = text.split(|&c| c == b':').map(|item| item.to_vec().into());
	(!text.is_empty()).then_some(iter).into_iter().flatten()
}

pub struct Swallow {
	immune_names: HashSet<Rc<[u8]>>,
	terminal_names: HashSet<Rc<[u8]>>,
	all_windows: Vec<Window>,
	parent_table: WeakValueHashMap<u32, Weak<Parent>, RandomState>,
	child_table: HashMap<Window, Child>,
}

impl Swallow {
	pub fn new(cx: &Context) -> xcb::Result<Self> {
		let mut immune_names = HashSet::default();
		let mut terminal_names = HashSet::default();
		terminal_names.insert(env_bytes("TERMINAL").into());
		terminal_names.extend(env_iter(&env_bytes("XSWALLOW_TERMINALS")));
		immune_names.extend(env_iter(&env_bytes("XSWALLOW_IMMUNE")));
		// wouldn't really make sense to swallow a terminal into a terminal
		immune_names.extend(terminal_names.iter().cloned());
		output::setup_state(&immune_names, &terminal_names);
		Ok(Self {
			immune_names,
			terminal_names,
			all_windows: cx.get_window_list()?.value().to_vec(),
			parent_table: WeakValueHashMap::default(),
			child_table: HashMap::default(),
		})
	}
	pub fn window_list(&mut self, cx: &Context) -> Option<Infallible> {
		let new_windows = cx.get_window_list().ok()?;
		let new_windows = new_windows.value::<Window>();
		list_diff(&mut self.all_windows, new_windows, |child_window| {
			let child_pid = cx.window_pid(child_window)?;
			let (parent_pid, child_name) = get_pid_info(child_pid)?;
			output::new_window(child_window, child_pid, &child_name);
			if !self.immune_names.contains(child_name.as_slice()) {
				return None;
			}
			let (parent_pid, parent_name) =
				find_parent(parent_pid, &self.immune_names, &self.terminal_names)?;
			let parent_window = cx.find_window_with_pid(parent_pid, new_windows)?;
			output::find_parent_success(parent_window, parent_pid, &parent_name);
			let (parent, position);
			match self.parent_table.entry(parent_pid) {
				WvhmEntry::Occupied(occupied) => {
					position = cx.get_window_geometry(child_window)?;
					parent = occupied.get_strong();
				}
				WvhmEntry::Vacant(vacant) => {
					position = cx.get_window_geometry(parent_window)?;
					parent = vacant.insert(Rc::new(Parent {
						window: parent_window,
						position,
					}));
					cx.hide_window(parent_window);
					cx.set_window_geometry(child_window, position);
				}
			}
			cx.subscribe(child_window);
			cx.flush();
			self.parent_table.insert(parent_pid, parent.clone());
			self.child_table.insert(child_window, Child {
				pid: child_pid,
				parent,
				position,
			});
			Some(())
		});
		None
	}
	pub fn update(&mut self, cx: &Context, win: Window) -> Option<Infallible> {
		self.child_table.get_mut(&win)?.position = cx.get_window_geometry(win)?;
		None
	}
	pub fn close(&mut self, cx: &Context, win: Window) -> Option<Infallible> {
		let Child {
			pid,
			parent,
			position,
		} = self.child_table.remove(&win)?;
		output::close_window(win, pid, Rc::strong_count(&parent));
		// no more child windows open
		if Rc::strong_count(&parent) == 1 {
			// specific order to prevent “not working”
			cx.set_window_geometry(parent.window, position);
			cx.show_window(parent.window);
			cx.set_window_active_if(win, parent.window);
			cx.set_window_geometry(parent.window, position);
			// not sure if i need this
			cx.flush();
		}
		None
	}
	pub fn quit(&mut self, cx: &Context) {
		// show all the windows that were hidden
		for parent in self.parent_table.values() {
			cx.set_window_geometry(parent.window, parent.position);
			cx.show_window(parent.window);
			cx.set_window_geometry(parent.window, parent.position);
		}
		// make sure requests actually get processed
		cx.flush_hard();
	}
}
