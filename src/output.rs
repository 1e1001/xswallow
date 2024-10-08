//! all printing is done to stderr, dunno if that's a good idea
#![allow(clippy::print_stderr, reason = "it's the printing code")]
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use foldhash::HashSet;
use xcb::Xid;
use xcb::x::{Atom, Window};

use crate::context::Geometry;

/// for better-looking outputs,
/// doesn't really matter since it's logs but i like nice logs
struct MiniDebug<T>(T);
impl<T: Xid> MiniDebug<T> {
	fn xid(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "0x{:x}", self.0.resource_id())
	}
}

macro_rules! fmt {
	($ty:ty, |$self:ident, $f:ident| $($tt:tt)*) => {
		impl fmt::Display for MiniDebug<$ty> {
			fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
				write!(f, "{self:?}")
			}
		}
		impl fmt::Debug for MiniDebug<$ty> {
			fn fmt(&$self, $f: &mut fmt::Formatter) -> fmt::Result { $($tt)* }
		}
	}
}

fmt!(Window, |self, f| self.xid(f));
fmt!(Atom, |self, f| self.xid(f));
fmt!(&[Atom], |self, f| {
	f.debug_list()
		.entries(self.0.iter().copied().map(MiniDebug))
		.finish()
});
fmt!(&HashSet<Rc<[u8]>>, |self, f| {
	f.debug_set()
		.entries(self.0.iter().map(|v| MiniDebug(&**v)))
		.finish()
});
// for displaying process names
fmt!(&[u8], |self, f| {
	write!(f, "\"")?;
	for chunk in self.0.utf8_chunks() {
		for c in chunk.valid().chars() {
			match c {
				'\'' => write!(f, "'")?,
				_ => write!(f, "{}", c.escape_debug())?,
			}
		}
		for b in chunk.invalid() {
			write!(f, "\\x{b:02x}")?;
		}
	}
	write!(f, "\"")
});

pub fn welcome() {
	eprintln!(concat!(
		"xswallow v",
		env!("CARGO_PKG_VERSION"),
		" by 1e1001"
	));
}

pub fn quit() {
	eprintln!("Quitting…");
}

pub fn error<E: Error>(e: E) {
	eprintln!("Error {e}\n{e:?}");
}

pub fn setup_context(screen: i32, window: Window, atoms: &[Atom]) {
	eprintln!("Root: {} / {}", screen, MiniDebug(window));
	eprintln!("Atoms: {}", MiniDebug(atoms));
}

pub fn setup_state(immune: &HashSet<Rc<[u8]>>, terminal: &HashSet<Rc<[u8]>>) {
	eprintln!("Terminal processes: {}", MiniDebug(terminal));
	eprintln!("Immune processes: {}", MiniDebug(immune));
}

pub fn window_vis(mode: bool, window: Window) {
	eprintln!(
		"- {} {}",
		if mode { "Showing" } else { "Hiding" },
		MiniDebug(window)
	);
}

pub fn window_move(window: Window, pos: Geometry) {
	eprintln!("- Moving {} to {}", MiniDebug(window), pos);
}

pub fn window_refocus(from: Window, to: Window) {
	eprintln!(
		"- Moving focus from {} to {}",
		MiniDebug(from),
		MiniDebug(to)
	);
}

pub fn flush_hard() {
	eprintln!("- Hard event flush");
}

pub fn find_next_parent(pid: u32, name: &[u8]) {
	eprintln!("  → {} {:?}", pid, MiniDebug(name));
}

pub fn new_window(win: Window, pid: u32, name: &[u8]) {
	eprintln!("New window: {} {} {}", MiniDebug(win), pid, MiniDebug(name));
}

pub fn find_parent_success(win: Window, pid: u32, name: &[u8]) {
	eprintln!("  Parent: {} {} {}", MiniDebug(win), pid, MiniDebug(name));
}

pub fn close_window(win: Window, pid: u32, remaining: usize) {
	eprintln!("Close window {} {}", MiniDebug(win), pid);
	eprintln!("  Remaining: {}", remaining - 1);
}
