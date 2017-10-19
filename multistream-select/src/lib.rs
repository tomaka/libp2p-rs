#[macro_use] extern crate error_chain;
extern crate integer_encoding as varint;

use std::io::{Read, Write};
use varint::{VarIntWriter};

static PROTOCOL_ID: &'static str = "/multistream/1.0.0";

error_chain! {
	foreign_links {
		Fmt(::std::fmt::Error);
		Io(::std::io::Error) #[cfg(unix)];
	}
}

type Matcher<'a> = Box<Fn(&str) -> bool + 'a>;
type HandlerFunc<T: Read + Write> = Box<Fn(&str, T) -> Result<()>>;

pub struct Handler<'a, T: Read + Write> {
	matcher: Matcher<'a>,
	handle: HandlerFunc<T>,
	name: String,
}

impl<'a, T: Read + Write> Handler<'a, T> {
	pub fn new(name: String, handler_func: HandlerFunc<T>) -> Handler<'a, T> {
		Handler {
			matcher: fulltext_matcher(name.clone()),
			handle: handler_func,
			name: name,
		}
	}
}

pub struct Multistream<'a, T: Read + Write> {
	handlers: Vec<Handler<'a, T>>
}
impl<'a, T: Read + Write> Multistream<'a, T> {
	pub fn new() -> Multistream<'a, T> {
		Multistream {
			handlers: vec![],
		}
	}

	pub fn add_handler(&mut self, h: Handler<'a, T>) {
		self.handlers.push(h);
	}

	pub fn ls_write(&self, mut stream: &mut T) -> Result<usize> {
		// write length of handlers
		let h_len = stream.write_varint(self.handlers.len())?;
		// write each handler on a new line
		let mut hs_len = 0;
		for handler in self.handlers.iter() {
			hs_len += delim_write(&mut stream, &handler.name.chars().map(|c| c as u8).collect::<Vec<u8>>())?;
		}
		Ok(h_len + hs_len)
	}
}

fn fulltext_matcher<'a>(a: String) -> Matcher<'a> {
	Box::new(move |b| a == b)
}

fn delim_write<T: Write>(w: &mut T, buf: &[u8]) -> Result<usize> {
	let v_len = w.write_varint(buf.len() + 1)?;
	let buf_len = w.write(buf)?;
	let n_len = w.write(&['\n' as u8])?;
	Ok(v_len + buf_len + n_len)
}

#[cfg(test)]
mod tests {
	use super::{Multistream, Handler};
	use std::io::Cursor;

	type Stream = Cursor<Vec<u8>>; // For testing

	#[test]
	fn multistream_prints_correct_handlers() {
		let mut ms = Multistream::new();
		let h: Handler<Stream> = Handler::new("/cats".to_string(), Box::new(|_, _| Ok(())));
		ms.add_handler(h);
		let mut buf = Default::default();
		ms.ls_write(&mut buf).unwrap();
		assert_eq!(buf.into_inner(), [1, 6, 47, 99, 97, 116, 115, 10]);
	}
}
