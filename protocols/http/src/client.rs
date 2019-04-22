use std::mem;

use atoi::atoi;
use bytes::{Bytes, BytesMut};
use http::{header, response, Request, Response};
use tokio_codec::{Decoder, Encoder};

use crate::error;

#[derive(Debug)]
pub struct HttpCodec {
    state: State,
}

impl HttpCodec {
    pub fn new() -> HttpCodec {
        HttpCodec {
            state: State::ParsingResponse,
        }
    }
}

#[derive(Debug)]
enum State {
    ParsingResponse,
    ReadingBody(response::Parts, usize),
}

impl Encoder for HttpCodec {
    type Item = Request<Bytes>;
    type Error = error::Error;

    fn encode(&mut self, item: Request<Bytes>, dst: &mut BytesMut) -> error::Result<()> {
        dst.extend_from_slice(item.method().as_str().as_bytes());
        dst.extend_from_slice(b" ");
        dst.extend_from_slice(item.uri().path().as_bytes());
        dst.extend_from_slice(b" HTTP/1.1\r\n");

        for (k, v) in item.headers() {
            dst.extend_from_slice(k.as_str().as_bytes());
            dst.extend_from_slice(b": ");
            dst.extend_from_slice(v.as_bytes());
            dst.extend_from_slice(b"\r\n");
        }

        dst.extend_from_slice(b"\r\n");
        dst.extend_from_slice(item.body());

        Ok(())
    }
}

impl Decoder for HttpCodec {
    type Item = Response<Bytes>;
    type Error = error::Error;

    fn decode(&mut self, src: &mut BytesMut) -> error::Result<Option<Response<Bytes>>> {
        use self::State::*;

        if src.len() == 0 {
            return Ok(None);
        }

        loop {
            match mem::replace(&mut self.state, ParsingResponse) {
                ParsingResponse => {
                    let amt = {
                        let mut headers = [httparse::EMPTY_HEADER; 16];
                        let mut response = httparse::Response::new(&mut headers);
                        let amt = match response.parse(src)? {
                            httparse::Status::Complete(amt) => amt,
                            httparse::Status::Partial => return Ok(None),
                        };
                        match response.version.unwrap() {
                            1 => (),
                            version => return Err(error::Error::VersionError(version)),
                        }
                        let mut builder = Response::builder();
                        builder.status(response.code.unwrap());
                        for header in response.headers.iter() {
                            builder.header(header.name, header.value);
                        }
                        let r = builder.body(()).unwrap();
                        let cl = match r.headers().get(header::CONTENT_LENGTH) {
                            Some(cl) => match atoi(cl.as_bytes()) {
                                Some(cl) => cl,
                                None => return Err(error::Error::ContentLengthError),
                            },
                            None => return Err(error::Error::ContentLengthError),
                        };
                        let (parts, _) = r.into_parts();
                        self.state = ReadingBody(parts, cl);
                        amt
                    };
                    src.advance(amt);
                }
                ReadingBody(parts, cl) => {
                    if src.len() < cl {
                        self.state = ReadingBody(parts, cl);
                        return Ok(None);
                    }
                    let body = src.split_to(cl).freeze();
                    return Ok(Some(Response::from_parts(parts, body)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate fake_stream;
    extern crate futures;

    use self::fake_stream::FakeStream;
    use self::futures::{Async, Future, Sink, Stream};
    use super::*;
    use http::{header, Method};
    use std::io::{Read, Write};
    use std::str::from_utf8;

    #[test]
    fn test_decode() {
        let mut fake = FakeStream::new();
        let res = b"\
            HTTP/1.1 200 OK\r\n\
            Date: Mon, 27 Jul 2009 12:28:53 GMT\r\n\
            Server: Apache/2.2.14 (Win32)\r\n\
            Last-Modified: Wed, 22 Jul 2009 19:15:56 GMT\r\n\
            Content-Length: 9\r\n\
            Content-Type: text/html\r\n\r\n\
            something";
        let wl = fake.write(res).unwrap();

        assert_eq!(res.len(), wl);

        let mut framed = HttpCodec::new().framed(fake);

        let response = match framed.poll().unwrap() {
            Async::Ready(Some(response)) => response,
            _ => panic!("no response"),
        };

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.headers().len(), 5);
        assert_eq!(response.body(), &Bytes::from_static(b"something"));
    }

    #[test]
    fn test_encode() {
        let fake = FakeStream::new();
        let expected = "\
                        POST /cgi-bin/process.cgi HTTP/1.1\r\n\
                        connection: Keep-Alive\r\n\
                        content-length: 9\r\n\r\n\
                        something";

        let mut buf = vec![0; expected.len()];

        let req = Request::builder()
            .method(Method::POST)
            .uri("/cgi-bin/process.cgi")
            .header(header::CONNECTION, "Keep-Alive")
            .header(header::CONTENT_LENGTH, 9)
            .body(Bytes::from_static(b"something"))
            .unwrap();

        let framed = HttpCodec::new().framed(fake);

        let framed = framed.send(req).wait().unwrap();

        let mut fake = framed.into_inner();

        let rl = fake.read(&mut buf).unwrap();

        assert_eq!(rl, expected.len());
        assert_eq!(from_utf8(&buf).unwrap(), expected);
    }
}
