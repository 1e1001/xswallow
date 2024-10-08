//! This is a binary crate

use context::{Context, Event};
use swallow::Swallow;

mod context;
mod output;
mod swallow;

fn main() -> xcb::Result<()> {
	output::welcome();
	let cx = Context::new()?;
	let mut swallow = Swallow::new(&cx)?;
	loop {
		match cx.next_event() {
			Event::Interrupted => None,
			Event::Quit => {
				swallow.quit(&cx);
				break;
			}
			Event::WindowList => swallow.window_list(&cx),
			Event::Update(win) => swallow.update(&cx, win),
			Event::Close(win) => swallow.close(&cx, win),
		};
	}
	Ok(())
}
