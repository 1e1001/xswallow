//! This is a binary crate, see the README for documentation

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, IsTerminal, stdout};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use clap::Parser;
use context::{Builder, RawEvent};
use control::{CtrlEvent, send_control_event};
use multiline_logger::log::{self, LevelFilter};

mod atom;
mod bell;
mod config;
mod context;
mod control;
mod swallow;

#[derive(Default, Debug)]
struct ConfigFile {
	section: u8,
	control: PathBuf,
	swallow: Option<swallow::Config>,
	bell: Option<bell::Config>,
}

fn parse_config(path: &Path) -> ConfigFile {
	#[expect(
		non_local_definitions,
		reason = "placed here to avoid polluting the imports"
	)]
	impl config::Config for ConfigFile {
		fn property(&mut self, line: &[u8]) -> Result<(), config::ParseError> {
			const SECTION_NONE: u8 = 0;
			const SECTION_SWALLOW: u8 = 1;
			const SECTION_BELL: u8 = 2;
			if config::key_then(line, b":").is_some() {
				self.section = SECTION_NONE;
			} else if let Some(enable) = config::key_then(line, b"swallow:") {
				self.swallow = (config::value_integer(enable?)? > 0).then(Default::default);
				self.section = SECTION_SWALLOW;
			} else if let Some(enable) = config::key_then(line, b"bell:") {
				self.bell = (config::value_integer(enable?)? > 0).then(Default::default);
				self.section = SECTION_BELL;
			} else if self.section == SECTION_NONE {
				if let Some(control) = config::key_then(line, b"control:") {
					self.control = OsString::from_vec(config::value_text(control?)).into();
				}
			} else if self.section == SECTION_SWALLOW {
				if let Some(swallow) = &mut self.swallow {
					swallow.property(line)?;
				}
			} else if self.section == SECTION_BELL {
				if let Some(bell) = &mut self.bell {
					bell.property(line)?;
				}
			} else {
				return Err(config::ParseError::InvalidPropertyName);
			}
			Ok(())
		}
	}
	config::parse_file::<ConfigFile, _>(BufReader::new(
		File::open(path).expect("Failed to open config file"),
	))
	.expect("Failed to read config")
}

fn parse_hex(text: &str) -> Result<u32, config::ParseError> {
	config::value_integer(text.as_bytes())
}

#[derive(Parser, Debug)]
// TODO: is author even mentioned anywhere? i don't see it in the help text
#[command(author, about, long_about = None)]
enum Args {
	/// Start the daemon
	Start {
		/// Path to the configuration file
		config: PathBuf,
	},
	/// Print the example configuration to stdout
	Config,
	/// Toggle a window's swallow status
	#[command(name = "swallow:toggle")]
	SwallowToggle {
		/// Path to the control socket to connect to
		#[arg(short, long)]
		control: Option<PathBuf>,
		/// Use a specific window id instead of a window picker
		#[arg(long, value_parser = parse_hex)]
		id: Option<u32>,
	},
	/// Manually ring the bell without going through the X server
	#[command(name = "bell:ring")]
	BellRing {
		/// Path to the control socket to connect to
		#[arg(short, long)]
		control: Option<PathBuf>,
	},
}

#[tokio::main(flavor = "current_thread")]
async fn run_start(config: &Path) {
	multiline_logger::Settings {
		title: "xextra",
		filters: &[("", LevelFilter::Trace)],
		file_out: None,
		console_out: true,
		panic_hook: true,
	}
	.init();
	let config = parse_config(config);
	log::debug!("Config: {config:?}");
	let mut cx = Builder::new();
	let bell = cx.pre_init::<bell::State>(config.bell);
	let swallow = cx.pre_init::<swallow::State>(config.swallow);
	let mut cx = cx.build(config.control);
	let mut bell = cx.init(bell);
	let mut swallow = cx.init(swallow);
	loop {
		let event = cx.next_event().await;
		// TODO: log in more specific places
		log::trace!("Event {event:?}");
		cx.event(&event, &mut bell);
		cx.event(&event, &mut swallow);
		if let RawEvent::Quit = event {
			break;
		}
	}
}

fn main() {
	match Args::parse() {
		Args::Start { config } => run_start(&config),
		#[expect(clippy::print_stdout, reason = "it's printing")]
		Args::Config => {
			println!(include_str!("example.conf"));
			// warning message if the text isn't going to a file.
			// if the terminal doesn't process escape sequences
			// then you get to muse your shell :)
			if stdout().is_terminal() {
				println!(
					"\x1b[1muse your shell to pipe this into a file, don't just copy from the terminal\x1b[0m"
				);
			}
		}
		Args::SwallowToggle { control, id } => {
			let id = id.unwrap_or_else(swallow::window_picker);
			send_control_event(control, &CtrlEvent::SwallowToggle(id));
		}
		Args::BellRing { control } => send_control_event(control, &CtrlEvent::BellRing),
	}
}
