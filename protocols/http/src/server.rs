use std::mem;

use atoi::atoi;
use bytes::{Bytes, BytesMut};
use http::{header, request, Request, Response};
use tokio_codec::{Decoder, Encoder};

use crate::error;

#[derive(Debug)]
pub struct HttpCodec {
    state: State,
    max_cl: usize,
}

impl HttpCodec {
    pub fn new(max_cl: usize) -> HttpCodec {
        HttpCodec {
            state: State::ParsingRequest,
            max_cl,
        }
    }
}

#[derive(Debug)]
enum State {
    ParsingRequest,
    ReadingBody(request::Parts, usize),
}

impl Encoder for HttpCodec {
    type Item = Response<Bytes>;
    type Error = error::Error;

    fn encode(&mut self, item: Response<Bytes>, dst: &mut BytesMut) -> error::Result<()> {
        dst.extend_from_slice(b"HTTP/1.1 ");
        dst.extend_from_slice(format!("{}", item.status()).as_bytes());
        dst.extend_from_slice(b"\r\n");

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
    type Item = Request<Bytes>;
    type Error = error::Error;

    fn decode(&mut self, src: &mut BytesMut) -> error::Result<Option<Request<Bytes>>> {
        use self::State::*;

        if src.len() == 0 {
            return Ok(None);
        }

        loop {
            match mem::replace(&mut self.state, ParsingRequest) {
                ParsingRequest => {
                    let amt = {
                        let mut headers = [httparse::EMPTY_HEADER; 16];
                        let mut request = httparse::Request::new(&mut headers);
                        let amt = match request.parse(src)? {
                            httparse::Status::Complete(amt) => amt,
                            httparse::Status::Partial => return Ok(None),
                        };
                        match request.version.unwrap() {
                            1 => (),
                            version => return Err(error::Error::VersionError(version)),
                        }
                        let mut builder = Request::builder();
                        builder.method(request.method.unwrap());
                        builder.uri(request.path.unwrap());
                        for header in request.headers.iter() {
                            builder.header(header.name, header.value);
                        }
                        let r = builder.body(()).unwrap();
                        let cl = match r.headers().get(header::CONTENT_LENGTH) {
                            Some(cl) => match atoi(cl.as_bytes()) {
                                Some(cl) => cl,
                                None => return Err(error::Error::ContentLengthError),
                            },
                            None => 0,//return Err(error::Error::ContentLengthError),
                        };
                        if cl > self.max_cl {
                            return Err(error::Error::ContentLengthError);
                        }
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
                    return Ok(Some(Request::from_parts(parts, body)));
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
    use http::{status, Method};
    use std::io::{Read, Write};
    use std::str::from_utf8;

    #[test]
    fn test_decode() {
        let mut fake = FakeStream::new();
        let req = b"\
                    POST /cgi-bin/process.cgi HTTP/1.1\r\n\
                    connection: Keep-Alive\r\n\
                    content-length: 9\r\n\r\n\
                    something";
        let wl = fake.write(req).unwrap();

        assert_eq!(req.len(), wl);

        let mut framed = HttpCodec::new(10).framed(fake);

        let request = match framed.poll().unwrap() {
            Async::Ready(Some(request)) => request,
            _ => panic!("no request"),
        };

        assert_eq!(request.uri().path(), "/cgi-bin/process.cgi");
        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.headers().len(), 2);
        assert_eq!(request.body(), &Bytes::from_static(b"something"));
    }

    #[test]
    fn test_encode() {
        let fake = FakeStream::new();
        let expected = "\
                        HTTP/1.1 200 OK\r\n\
                        content-length: 9\r\n\
                        content-type: text/html\r\n\r\n\
                        something";

        let mut buf = vec![0; expected.len()];

        let res = Response::builder()
            .status(status::StatusCode::OK)
            .header("content-length", "9")
            .header("content-type", "text/html")
            .body(Bytes::from_static(b"something"))
            .unwrap();

        let framed = HttpCodec::new(10).framed(fake);

        let framed = framed.send(res).wait().unwrap();

        let mut fake = framed.into_inner();

        let rl = fake.read(&mut buf).unwrap();

        assert_eq!(rl, expected.len());
        assert_eq!(from_utf8(&buf).unwrap(), expected);
    }
}
