//! Audio bell
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use claxon::FlacReader;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ChannelCount, Device, SampleFormat, SampleRate, SizedSample, Stream, StreamConfig};
use hound::WavReader;
use lewton::inside_ogg::OggStreamReader;
use multiline_logger::log;
use tokio::sync::mpsc::Sender;
use xcb::xkb;

use crate::config;
use crate::context::{Builder, Component, Context, Extension, RawEvent};
use crate::control::CtrlEvent;

#[derive(Default, Debug)]
pub struct Config {
	pub path: PathBuf,
	pub volume: f32,
}

impl config::Config for Config {
	fn property(&mut self, line: &[u8]) -> Result<(), config::ParseError> {
		if let Some(path) = config::key_then(line, b"path:") {
			self.path = OsString::from_vec(config::value_text(path?)).into();
		} else if let Some(volume) = config::key_then(line, b"volume:") {
			self.volume = config::value_float(volume?)?;
		} else {
			return Err(config::ParseError::InvalidPropertyName);
		}
		Ok(())
	}
}

struct AudioThread {
	stream: Stream,
}

struct Output {
	device: Device,
	volume: f32,
	tx: Sender<RawEvent>,
}

// since this'll never be initialized more than once per process (hopefully)
// i can use statics just fine
static RESET: AtomicBool = AtomicBool::new(false);

impl AudioThread {
	fn get_config(
		device: &Device,
		format: SampleFormat,
		rate: SampleRate,
		channels: ChannelCount,
	) -> StreamConfig {
		device
			.supported_output_configs()
			.expect("Bad output device")
			.find_map(|config| {
				if config.sample_format() != format || config.channels() != channels {
					None
				} else {
					config.try_with_sample_rate(rate)
				}
			})
			.expect("No matching output config")
			.config()
	}
	fn with_data<S>(
		output: Output,
		rate: SampleRate,
		channels: ChannelCount,
		mut samples: Box<[S]>,
	) -> Self
	where
		S: SizedSample<Float = f32> + Copy + Send + 'static,
	{
		// pre-fade samples
		for sample in &mut samples {
			*sample = (*sample).mul_amp(output.volume);
		}
		let config = Self::get_config(&output.device, S::FORMAT, rate, channels);
		// fade duration
		let fade_len = (rate.0 / 300) as usize;
		let mut index = 0;
		let mut timeout = 0;
		let stream = output
			.device
			.build_output_stream::<S, _, _>(
				&config,
				move |mut data, _| {
					if RESET.swap(false, Ordering::Relaxed) {
						// play a fadeout to reduce clicking sound
						let tail = &samples[index..];
						let size = tail.len().min(data.len()).min(fade_len);
						data[..size].copy_from_slice(&tail[..size]);
						#[expect(clippy::cast_precision_loss, reason = "audio code :)")]
						for (i, sample) in data[..size].iter_mut().enumerate() {
							*sample = (*sample).mul_amp((fade_len - i) as f32 / fade_len as f32);
						}
						// play the next bell after this fade
						data = &mut data[size..];
						index = 0;
					}
					let tail = &samples[index..];
					let size = tail.len().min(data.len());
					data[..size].copy_from_slice(&tail[..size]);
					data[size..].fill(S::EQUILIBRIUM);
					index += size;
					// this will run every frame the sample is done
					if index == samples.len() {
						if timeout == 0 {
							if output
								.tx
								.try_send(RawEvent::Control(CtrlEvent::BellSleep))
								.is_ok()
							{
								timeout = rate.0;
							}
						} else {
							timeout = timeout
								.saturating_sub(u32::try_from(data.len()).unwrap_or(u32::MAX));
						}
					}
				},
				|err| log::error!("Audio error: {err}\n{err:?}"),
				None,
			)
			.expect("Failed to open audio stream");
		Self { stream }
	}
	fn with_ogg<T: Read + Seek>(output: Output, mut ogg: OggStreamReader<T>) -> Self {
		use lewton::header::IdentHeader;
		use lewton::samples::InterleavedSamples;
		let IdentHeader {
			audio_channels,
			audio_sample_rate,
			..
		} = ogg.ident_hdr;
		// seems ogg doesn't specify a sample format, ffmpeg calls it fltp
		let mut samples = Vec::new();
		while let Some(packet) = ogg
			.read_dec_packet_generic::<InterleavedSamples<f32>>()
			.expect("Invalid audio data")
		{
			samples.extend_from_slice(&packet.samples);
		}
		Self::with_data::<f32>(
			output,
			SampleRate(audio_sample_rate),
			audio_channels.into(),
			samples.into_boxed_slice(),
		)
	}
	fn new(output: Output, path: &PathBuf) -> Self {
		fn reset<T: Read + Seek>(mut reader: T) -> T {
			reader.rewind().unwrap();
			reader
		}
		if path.as_os_str().is_empty() {
			// A nice builtin bell sound, thanks to arsentical for making this 6 years ago
			return Self::with_ogg(
				output,
				OggStreamReader::new(Cursor::new(include_bytes!("bell.ogg"))).unwrap(),
			);
		}
		let file = File::open(path).expect("Failed to open file");
		if let Ok(wav) = WavReader::new(BufReader::new(&file)) {
			fn samples<S: hound::Sample>(wav: WavReader<BufReader<&File>>) -> Box<[S]> {
				wav.into_samples::<S>()
					.collect::<Result<_, _>>()
					.expect("Invalid audio data")
			}
			let hound::WavSpec {
				channels,
				sample_rate,
				bits_per_sample,
				sample_format,
			} = wav.spec();
			let rate = SampleRate(sample_rate);
			match (bits_per_sample, sample_format) {
				(8, hound::SampleFormat::Int) => {
					Self::with_data::<i8>(output, rate, channels, samples(wav))
				}
				(16, hound::SampleFormat::Int) => {
					Self::with_data::<i16>(output, rate, channels, samples(wav))
				}
				(32, hound::SampleFormat::Int) => {
					Self::with_data::<i32>(output, rate, channels, samples(wav))
				}
				(32, hound::SampleFormat::Float) => {
					Self::with_data::<f32>(output, rate, channels, samples(wav))
				}
				_ => panic!("Unsupported wav: {:?}", wav.spec()),
			}
		} else if let Ok(flac) = FlacReader::new(reset(&file)) {
			use claxon::metadata::StreamInfo;
			fn samples<S>(mut flac: FlacReader<&File>, f: impl Fn(i32) -> S) -> Box<[S]> {
				// claxon's sample format sucks for reading block-wise so i just give up
				flac.samples().map(|s| f(s.unwrap())).collect()
			}
			let StreamInfo {
				sample_rate,
				channels,
				bits_per_sample,
				..
			} = flac.streaminfo();
			let rate = SampleRate(sample_rate);
			let channels = u16::try_from(channels).expect("Too many channels!");
			#[expect(
				clippy::cast_possible_truncation,
				reason = "library should validate this :)"
			)]
			match bits_per_sample {
				8 => Self::with_data::<i8>(output, rate, channels, samples(flac, |s| s as i8)),
				16 => Self::with_data::<i16>(output, rate, channels, samples(flac, |s| s as i16)),
				32 => Self::with_data::<i32>(output, rate, channels, samples(flac, |s| s)),
				_ => panic!("Unsupported flac: {:?}", flac.streaminfo()),
			}
		} else if let Ok(ogg) = OggStreamReader::new(BufReader::new(reset(&file))) {
			Self::with_ogg(output, ogg)
		} else {
			panic!("Invalid audio file!")
		}
	}
}

pub struct State {
	thread: Option<AudioThread>,
	config: Config,
}

impl State {
	fn reset_audio(&mut self, cx: &Context) {
		drop(self.thread.take());
		let device = cpal::default_host()
			.default_output_device()
			.expect("You have no audio output!");
		self.thread = Some(AudioThread::new(
			Output {
				device,
				volume: self.config.volume,
				tx: cx.new_tx(),
			},
			&self.config.path,
		));
	}
}

impl Component for State {
	type Config = Config;
	const NAME: &str = "bell";
	fn pre_init(cx: &mut Builder, config: Self::Config) -> Self {
		cx.require_extension(Extension::Xkb);
		Self {
			thread: None,
			config,
		}
	}
	fn init(&mut self, cx: &Context) -> xcb::Result<()> {
		self.reset_audio(cx);
		cx.connection.send_request(&xkb::SelectEvents {
			device_spec: xkb::Id::UseCoreKbd as u16,
			affect_which: xkb::EventType::BELL_NOTIFY,
			clear: xkb::EventType::empty(),
			select_all: xkb::EventType::BELL_NOTIFY,
			affect_map: xkb::MapPart::all(),
			map: xkb::MapPart::all(),
			details: &[],
		});
		cx.set_bell(false)
	}
	fn event(&mut self, cx: &Context, event: &RawEvent) -> xcb::Result<()> {
		match event {
			// TODO: take advantage of bell config (duration, pitch, percent (volume?), id)
			// bell event has no documentation though
			RawEvent::Control(CtrlEvent::BellRing)
			| RawEvent::X(xcb::Event::Xkb(xkb::Event::BellNotify(_))) => {
				if let Some(thread) = &mut self.thread {
					_ = thread.stream.play();
				}
				RESET.store(true, Ordering::Relaxed);
				Ok(())
			}
			RawEvent::Control(CtrlEvent::BellSleep) => {
				// TODO: only pause if count is zero
				if let Some(thread) = &mut self.thread {
					_ = thread.stream.pause();
				}
				Ok(())
			}
			RawEvent::Control(CtrlEvent::BellReset) => {
				self.reset_audio(cx);
				Ok(())
			}
			RawEvent::Quit => cx.set_bell(true),
			_ => Ok(()),
		}
	}
}

