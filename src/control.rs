//! control interface, serialization

// TODO: some sorta reverse-communication: send status back to event caller?

use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::os::unix::net as net_sync;
use std::path::PathBuf;

use multiline_logger::log;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net as net_async;

use crate::config::get_uid;

// protocol names
const COMMAND_SWALLOW_TOGGLE: [u8; 8] = *b"swaltogl";
const COMMAND_BELL_RING: [u8; 8] = *b"bellring";
const COMMAND_BELL_RESET: [u8; 8] = *b"bellrsta";

#[derive(Clone, Debug)]
pub enum CtrlEvent {
	// public events
	SwallowToggle(u32),
	BellRing,
	BellReset,
	// internal events
	BellSleep,
}

impl CtrlEvent {
	pub async fn read<R: AsyncRead + Unpin>(mut stream: R) -> io::Result<Self> {
		let mut buf = [0; 8];
		stream.read_exact(&mut buf).await?;
		Ok(match buf {
			COMMAND_SWALLOW_TOGGLE => {
				let mut buf2 = [0; 4];
				stream.read_exact(&mut buf2).await?;
				CtrlEvent::SwallowToggle(u32::from_ne_bytes(buf2))
			}
			COMMAND_BELL_RING => CtrlEvent::BellRing,
			COMMAND_BELL_RESET => CtrlEvent::BellReset,
			_ => {
				return Err(io::Error::new(
					io::ErrorKind::InvalidData,
					"Invalid control command",
				));
			}
		})
	}
	fn write(&self, mut stream: impl Write) -> io::Result<()> {
		match self {
			CtrlEvent::SwallowToggle(id) => {
				stream.write_all(&COMMAND_SWALLOW_TOGGLE)?;
				stream.write_all(&id.to_ne_bytes())?;
			}
			CtrlEvent::BellRing => {
				stream.write_all(&COMMAND_BELL_RING)?;
			}
			CtrlEvent::BellReset => {
				stream.write_all(&COMMAND_BELL_RESET)?;
			}
			CtrlEvent::BellSleep => unimplemented!(),
		}
		Ok(())
	}
}

pub async fn run_control_loop<R: Future<Output = ()>, F: FnMut(CtrlEvent) -> R>(
	path: PathBuf,
	mut f: F,
) {
	// delete the file if it's already there :)
	// should probably do some one-instance checking,
	// but that breaks as soon as it's improperly closed
	_ = fs::remove_file(&path);
	let listener = net_async::UnixListener::bind(path).expect("Failed to start listener");
	loop {
		let result = match listener.accept().await {
			Ok((stream, _)) => CtrlEvent::read(stream).await,
			Err(e) => Err(e),
		};
		match result {
			Ok(event) => f(event).await,
			Err(e) => log::error!("Recv error: {e}\n{e:?}"),
		}
	}
}

pub fn send_control_event(control: Option<PathBuf>, event: &CtrlEvent) {
	let control =
		control.unwrap_or_else(|| PathBuf::from(format!("/run/user/{}/xextra control", get_uid())));
	let stream = net_sync::UnixStream::connect(control).expect("Failed to connect to control");
	event.write(stream).expect("Failed to write event");
}
