//! Configuration file parser, and `/proc/{pid}/status`

use std::io::{self, BufRead};
use std::iter;
use std::str::from_utf8;
use std::vec::IntoIter as VecIntoIter;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
	#[error("Success?")]
	EarlyExit,
	#[error("Invalid line")]
	InvalidLine,
	#[error("Invalid property name")]
	InvalidPropertyName,
	#[error("Invalid number")]
	InvalidNumber,
	#[error("io Error")]
	IoError(#[from] io::Error),
}

pub trait Config: Default {
	fn property(&mut self, line: &[u8]) -> Result<(), ParseError>;
}

pub fn parse_file<T: Config, B: BufRead>(file: B) -> Result<T, ParseError> {
	let mut result = T::default();
	for line in file.split(b'\n') {
		let line = line.map_err(ParseError::IoError)?;
		let mut line = line.as_slice();
		while let [..=b' ', rest @ ..] = line {
			line = rest;
		}
		if matches!(line.first(), None | Some(b'#')) {
			continue;
		}
		match result.property(line) {
			Ok(()) => {}
			Err(ParseError::EarlyExit) => break,
			Err(err) => return Err(err),
		}
	}
	Ok(result)
}

fn line_value(tail: &[u8]) -> Result<&[u8], ParseError> {
	match tail {
		[..=b' ', value @ ..] => Ok(value),
		[] => Ok(&[]),
		_ => Err(ParseError::InvalidLine),
	}
}

/// make sure to add a `:` at the end of the property name
pub fn key_then<'line>(line: &'line [u8], name: &[u8]) -> Option<Result<&'line [u8], ParseError>> {
	line.strip_prefix(name).map(line_value)
}

pub fn value_integer(text: &[u8]) -> Result<u32, ParseError> {
	let text = from_utf8(text).map_err(|_| ParseError::InvalidNumber)?;
	if let Some(text) = text.strip_prefix('x').or_else(|| text.strip_prefix("0x")) {
		u32::from_str_radix(text, 16)
	} else {
		text.parse()
	}
	.map_err(|_| ParseError::InvalidNumber)
}

pub fn value_float(text: &[u8]) -> Result<f32, ParseError> {
	let text = from_utf8(text).map_err(|_| ParseError::InvalidNumber)?;
	text.parse().map_err(|_| ParseError::InvalidNumber)
}

// TODO: better way to organize this? i want to keep text parsing the same
// between value_text and value_text_list but they need to parse differently
// (and \u needs to still work!)

enum TextEscape {
	Char(u8),
	Text(VecIntoIter<u8>),
	Separator,
	None,
}

pub fn get_uid() -> u32 {
	// SAFETY: probably fine? this should just be a pure function
	unsafe { libc::getuid() }
}

impl TextEscape {
	fn new(iter: &mut impl Iterator<Item = u8>) -> Self {
		match iter.next() {
			Some(b'\\') => match iter.next() {
				Some(b'0') => Self::Char(b'\0'),
				Some(b'n') => Self::Char(b'\n'),
				Some(b'u') => Self::Text(get_uid().to_string().into_bytes().into_iter()),
				Some(char) => Self::Char(char),
				None => Self::None,
			},
			Some(b',') => Self::Separator,
			Some(char) => Self::Char(char),
			None => Self::None,
		}
	}
	fn not_separator(self) -> Option<Self> {
		match self {
			Self::None | Self::Separator => None,
			_ => Some(self),
		}
	}
	fn not_none(self) -> Option<Self> {
		match self {
			Self::None => None,
			_ => Some(self),
		}
	}
}

impl Iterator for TextEscape {
	type Item = u8;
	fn next(&mut self) -> Option<Self::Item> {
		match self {
			&mut Self::Char(char) => {
				*self = Self::None;
				Some(char)
			}
			Self::Text(iter) => iter.next(),
			Self::Separator => {
				*self = Self::None;
				Some(b',')
			}
			Self::None => None,
		}
	}
}

pub fn value_text(text: &[u8]) -> Vec<u8> {
	let mut iter = text.iter().copied();
	iter::from_fn(move || TextEscape::new(&mut iter).not_none())
		.flatten()
		.collect()
}

pub fn value_text_list(text: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
	let mut iter = text.iter().copied();
	// Thanks zachs18!
	// https://discord.com/channels/273534239310479360/1293715661016662137/1293717591671439431
	iter::from_fn(move || {
		let mut next = TextEscape::new(&mut iter).not_none()?;
		let mut buffer = Vec::new();
		while let Some(value) = next.not_separator() {
			buffer.extend(value);
			next = TextEscape::new(&mut iter);
		}
		Some(buffer)
	})
}
