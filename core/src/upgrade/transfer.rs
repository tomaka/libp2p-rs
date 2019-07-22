// Copyright 2019 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Contains some helper futures for creating upgrades.

use futures::prelude::*;
use std::{cmp, error, fmt, io::Cursor, mem, pin::Pin, task::Context, task::Poll};

mod io;

/// Send a message to the given socket, then shuts down the writing side.
///
/// > **Note**: Prepends a variable-length prefix indicate the length of the message. This is
/// >           compatible with what `read_one` expects.
#[inline]
pub fn write_one<TSocket, TData>(socket: TSocket, data: TData) -> WriteOne<TSocket, TData>
where
    TSocket: AsyncWrite + Unpin,
    TData: AsRef<[u8]>,
{
    let len_data = build_int_buffer(data.as_ref().len());
    WriteOne {
        inner: WriteOneInner::WriteLen(io::WriteAll::new(socket, len_data), data),
    }
}

/// Builds a buffer that contains the given integer encoded as variable-length.
fn build_int_buffer(num: usize) -> futures::io::Window<[u8; 10]> {
    let mut len_data = unsigned_varint::encode::u64_buffer();
    let encoded_len = unsigned_varint::encode::u64(num as u64, &mut len_data).len();
    let mut len_data = futures::io::Window::new(len_data);
    len_data.set(0..encoded_len);
    len_data
}

/// Future that makes `write_one` work.
pub struct WriteOne<TSocket, TData = Vec<u8>> {
    inner: WriteOneInner<TSocket, TData>,
}

enum WriteOneInner<TSocket, TData> {
    /// We need to write the data length to the socket.
    WriteLen(io::WriteAll<TSocket, futures::io::Window<[u8; 10]>>, TData),
    /// We need to write the actual data to the socket.
    Write(io::WriteAll<TSocket, TData>),
    /// We need to shut down the socket.
    Shutdown(io::Close<TSocket>),
    /// A problem happened during the processing.
    Poisoned,
}

impl<TSocket, TData> Future for WriteOne<TSocket, TData>
where
    TSocket: AsyncWrite + Unpin,
    TData: AsRef<[u8]>,
{
    type Output = Result<(), std::io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        Future::poll(Pin::new(&mut self.inner), cx)
            .map_ok(|_socket| ())
    }
}

impl<TSocket, TData> Unpin for WriteOneInner<TSocket, TData> {
}

impl<TSocket, TData> Future for WriteOneInner<TSocket, TData>
where
    TSocket: AsyncWrite + Unpin,
    TData: AsRef<[u8]>,
{
    type Output = Result<TSocket, std::io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        loop {
            match mem::replace(&mut *self, WriteOneInner::Poisoned) {
                WriteOneInner::WriteLen(mut inner, data) => match Future::poll(Pin::new(&mut inner), cx) {
                    Poll::Ready(Ok((socket, _))) => {
                        *self = WriteOneInner::Write(io::WriteAll::new(socket, data));
                    }
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                    Poll::Pending => {
                        *self = WriteOneInner::WriteLen(inner, data);
                    }
                },
                WriteOneInner::Write(mut inner) => match Future::poll(Pin::new(&mut inner), cx) {
                    Poll::Ready(Ok((socket, _))) => {
                        *self = WriteOneInner::Shutdown(io::Close::new(socket));
                    }
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                    Poll::Pending => {
                        *self = WriteOneInner::Write(inner);
                    }
                },
                WriteOneInner::Shutdown(ref mut inner) => return Future::poll(Pin::new(inner), cx),
                WriteOneInner::Poisoned => panic!(),
            }
        }
    }
}

/// Reads a message from the given socket. Only one message is processed and the socket is dropped,
/// because we assume that the socket will not send anything more.
///
/// The `max_size` parameter is the maximum size in bytes of the message that we accept. This is
/// necessary in order to avoid DoS attacks where the remote sends us a message of several
/// gigabytes.
///
/// > **Note**: Assumes that a variable-length prefix indicates the length of the message. This is
/// >           compatible with what `write_one` does.
#[inline]
pub fn read_one<TSocket>(
    socket: TSocket,
    max_size: usize,
) -> ReadOne<TSocket>
{
    ReadOne {
        inner: ReadOneInner::ReadLen {
            socket,
            len_buf: Cursor::new([0; 10]),
            max_size,
        },
    }
}

/// Future that makes `read_one` work.
pub struct ReadOne<TSocket> {
    inner: ReadOneInner<TSocket>,
}

enum ReadOneInner<TSocket> {
    // We need to read the data length from the socket.
    ReadLen {
        socket: TSocket,
        /// A small buffer where we will right the variable-length integer representing the
        /// length of the actual packet.
        len_buf: Cursor<[u8; 10]>,
        max_size: usize,
    },
    // We need to read the actual data from the socket.
    ReadRest(io::ReadExact<TSocket, futures::io::Window<Vec<u8>>>),
    /// A problem happened during the processing.
    Poisoned,
}

impl<TSocket> Future for ReadOne<TSocket>
where
    TSocket: AsyncRead + Unpin,
{
    type Output = Result<Vec<u8>, ReadOneError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        Future::poll(Pin::new(&mut self.inner), cx).map_ok(|(_, out)| out)
    }
}

impl<TSocket> Future for ReadOneInner<TSocket>
where
    TSocket: AsyncRead + Unpin,
{
    type Output = Result<(TSocket, Vec<u8>), ReadOneError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        loop {
            match mem::replace(&mut *self, ReadOneInner::Poisoned) {
                ReadOneInner::ReadLen {
                    mut socket,
                    mut len_buf,
                    max_size,
                } => {
                    match AsyncRead::poll_read(Pin::new(&mut socket), cx, len_buf.get_mut()) {
                        Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
                        Poll::Ready(Ok(num_read)) => {
                            // Reaching EOF before finishing to read the length is an error, unless
                            // the EOF is at the very beginning of the substream, in which case we
                            // assume that the data is empty.
                            if num_read == 0 {
                                if len_buf.position() == 0 {
                                    return Poll::Ready(Ok((socket, Vec::new())));
                                } else {
                                    return Poll::Ready(Err(ReadOneError::Io(
                                        std::io::ErrorKind::UnexpectedEof.into(),
                                    )));
                                }
                            }

                            let len_buf_with_data =
                                &len_buf.get_ref()[..len_buf.position() as usize];
                            if let Ok((len, data_start)) =
                                unsigned_varint::decode::usize(len_buf_with_data)
                            {
                                if len >= max_size {
                                    return Poll::Ready(Err(ReadOneError::TooLarge {
                                        requested: len,
                                        max: max_size,
                                    }));
                                }

                                // Create `data_buf` containing the start of the data that was
                                // already in `len_buf`.
                                let n = cmp::min(data_start.len(), len);
                                let mut data_buf = vec![0; len];
                                data_buf[.. n].copy_from_slice(&data_start[.. n]);
                                let mut data_buf = futures::io::Window::new(data_buf);
                                data_buf.set(data_start.len()..);
                                *self = ReadOneInner::ReadRest(io::ReadExact::new(socket, data_buf));
                            } else {
                                *self = ReadOneInner::ReadLen {
                                    socket,
                                    len_buf,
                                    max_size,
                                };
                            }
                        }
                        Poll::Pending => {
                            *self = ReadOneInner::ReadLen {
                                socket,
                                len_buf,
                                max_size,
                            };
                            return Poll::Pending;
                        }
                    }
                }
                ReadOneInner::ReadRest(mut inner) => {
                    match Future::poll(Pin::new(&mut inner), cx) {
                        Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
                        Poll::Ready(Ok((socket, data))) => {
                            return Poll::Ready(Ok((socket, data.into_inner())));
                        }
                        Poll::Pending => {
                            *self = ReadOneInner::ReadRest(inner);
                            return Poll::Pending;
                        }
                    }
                }
                ReadOneInner::Poisoned => panic!(),
            }
        }
    }
}

/// Error while reading one message.
#[derive(Debug)]
pub enum ReadOneError {
    /// Error on the socket.
    Io(std::io::Error),
    /// Requested data is over the maximum allowed size.
    TooLarge {
        /// Size requested by the remote.
        requested: usize,
        /// Maximum allowed.
        max: usize,
    },
}

impl From<std::io::Error> for ReadOneError {
    #[inline]
    fn from(err: std::io::Error) -> ReadOneError {
        ReadOneError::Io(err)
    }
}

impl fmt::Display for ReadOneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ReadOneError::Io(ref err) => write!(f, "{}", err),
            ReadOneError::TooLarge { .. } => write!(f, "Received data size over maximum"),
        }
    }
}

impl error::Error for ReadOneError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            ReadOneError::Io(ref err) => Some(err),
            ReadOneError::TooLarge { .. } => None,
        }
    }
}

/// Similar to `read_one`, but applies a transformation on the output buffer.
///
/// > **Note**: The `param` parameter is an arbitrary value that will be passed back to `then`.
/// >           This parameter is normally not necessary, as we could just pass a closure that has
/// >           ownership of any data we want. In practice, though, this would make the
/// >           `ReadRespond` type impossible to express as a concrete type. Once the `impl Trait`
/// >           syntax is allowed within traits, we can remove this parameter.
#[inline]
pub fn read_one_then<TSocket, TParam, TThen, TOut, TErr>(
    socket: TSocket,
    max_size: usize,
    param: TParam,
    then: TThen,
) -> ReadOneThen<TSocket, TParam, TThen>
where
    TSocket: AsyncRead + Unpin,
    TThen: FnOnce(Vec<u8>, TParam) -> Result<TOut, TErr>,
    TErr: From<ReadOneError>,
{
    ReadOneThen {
        inner: read_one(socket, max_size),
        then: Some((param, then)),
    }
}

/// Future that makes `read_one_then` work.
pub struct ReadOneThen<TSocket, TParam, TThen> {
    inner: ReadOne<TSocket>,
    then: Option<(TParam, TThen)>,
}

impl<TSocket, TParam, TThen> Unpin for ReadOneThen<TSocket, TParam, TThen> {
}

impl<TSocket, TParam, TThen, TOut, TErr> Future for ReadOneThen<TSocket, TParam, TThen>
where
    TSocket: AsyncRead + Unpin,
    TThen: FnOnce(Vec<u8>, TParam) -> Result<TOut, TErr>,
    TErr: From<ReadOneError>,
{
    type Output = Result<TOut, TErr>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        match Future::poll(Pin::new(&mut self.inner), cx) {
            Poll::Ready(Ok(buffer)) => {
                let (param, then) = self.then.take()
                    .expect("Future was polled after it was finished");
                Poll::Ready(then(buffer, param))
            },
            Poll::Ready(Err(err)) => Poll::Ready(Err(err.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Similar to `read_one`, but applies a transformation on the output buffer.
///
/// > **Note**: The `param` parameter is an arbitrary value that will be passed back to `then`.
/// >           This parameter is normally not necessary, as we could just pass a closure that has
/// >           ownership of any data we want. In practice, though, this would make the
/// >           `ReadRespond` type impossible to express as a concrete type. Once the `impl Trait`
/// >           syntax is allowed within traits, we can remove this parameter.
#[inline]
pub fn read_respond<TSocket, TThen, TParam, TOut, TErr>(
    socket: TSocket,
    max_size: usize,
    param: TParam,
    then: TThen,
) -> ReadRespond<TSocket, TParam, TThen>
where
    TSocket: AsyncRead,
    TThen: FnOnce(TSocket, Vec<u8>, TParam) -> Result<TOut, TErr>,
    TErr: From<ReadOneError>,
{
    ReadRespond {
        inner: read_one(socket, max_size).inner,
        then: Some((then, param)),
    }
}

/// Future that makes `read_respond` work.
pub struct ReadRespond<TSocket, TParam, TThen> {
    inner: ReadOneInner<TSocket>,
    then: Option<(TThen, TParam)>,
}

impl<TSocket, TParam, TThen> Unpin for ReadRespond<TSocket, TParam, TThen> {
}

impl<TSocket, TThen, TParam, TOut, TErr> Future for ReadRespond<TSocket, TParam, TThen>
where
    TSocket: AsyncRead + Unpin,
    TThen: FnOnce(TSocket, Vec<u8>, TParam) -> Result<TOut, TErr>,
    TErr: From<ReadOneError>,
{
    type Output = Result<TOut, TErr>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        match Future::poll(Pin::new(&mut self.inner), cx) {
            Poll::Ready(Ok((socket, buffer))) => {
                let (then, param) = self.then.take().expect("Future was polled after it was finished");
                Poll::Ready(Ok(then(socket, buffer, param)?))
            },
            Poll::Ready(Err(err)) => Poll::Ready(Err(err.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Send a message to the given socket, then shuts down the writing side, then reads an answer.
///
/// This combines `write_one` followed with `read_one_then`.
///
/// > **Note**: The `param` parameter is an arbitrary value that will be passed back to `then`.
/// >           This parameter is normally not necessary, as we could just pass a closure that has
/// >           ownership of any data we want. In practice, though, this would make the
/// >           `ReadRespond` type impossible to express as a concrete type. Once the `impl Trait`
/// >           syntax is allowed within traits, we can remove this parameter.
#[inline]
pub fn request_response<TSocket, TData, TParam, TThen, TOut, TErr>(
    socket: TSocket,
    data: TData,
    max_size: usize,
    param: TParam,
    then: TThen,
) -> RequestResponse<TSocket, TParam, TThen, TData>
where
    TSocket: AsyncRead + AsyncWrite + Unpin,
    TData: AsRef<[u8]>,
    TThen: FnOnce(Vec<u8>, TParam) -> Result<TOut, TErr>,
{
    RequestResponse {
        inner: RequestResponseInner::Write(write_one(socket, data).inner, max_size, param, then),
    }
}

/// Future that makes `request_response` work.
pub struct RequestResponse<TSocket, TParam, TThen, TData = Vec<u8>> {
    inner: RequestResponseInner<TSocket, TData, TParam, TThen>,
}

enum RequestResponseInner<TSocket, TData, TParam, TThen> {
    // We need to write data to the socket.
    Write(WriteOneInner<TSocket, TData>, usize, TParam, TThen),
    // We need to read the message.
    Read(ReadOneThen<TSocket, TParam, TThen>),
    // An error happened during the processing.
    Poisoned,
}

impl<TSocket, TParam, TThen, TData> Unpin for RequestResponse<TSocket, TParam, TThen, TData> {
}

impl<TSocket, TData, TParam, TThen, TOut, TErr> Future for RequestResponse<TSocket, TParam, TThen, TData>
where
    TSocket: AsyncRead + AsyncWrite + Unpin,
    TData: AsRef<[u8]>,
    TThen: FnOnce(Vec<u8>, TParam) -> Result<TOut, TErr>,
    TErr: From<ReadOneError>,
{
    type Output = Result<TOut, TErr>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        loop {
            match mem::replace(&mut self.inner, RequestResponseInner::Poisoned) {
                RequestResponseInner::Write(mut inner, max_size, param, then) => {
                    match Future::poll(Pin::new(&mut inner), cx).map_err(ReadOneError::Io)? {
                        Poll::Ready(socket) => {
                            self.inner =
                                RequestResponseInner::Read(read_one_then(socket, max_size, param, then));
                        }
                        Poll::Pending => {
                            self.inner = RequestResponseInner::Write(inner, max_size, param, then);
                            return Poll::Pending;
                        }
                    }
                }
                RequestResponseInner::Read(mut inner) => match Future::poll(Pin::new(&mut inner), cx)? {
                    Poll::Ready(packet) => return Poll::Ready(Ok(packet)),
                    Poll::Pending => {
                        self.inner = RequestResponseInner::Read(inner);
                        return Poll::Pending;
                    }
                },
                RequestResponseInner::Poisoned => panic!(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    #[test]
    fn write_one_works() {
        let data = (0..rand::random::<usize>() % 10_000)
            .map(|_| rand::random::<u8>())
            .collect::<Vec<_>>();

        let mut out = vec![0; 10_000];
        let future = write_one(Cursor::new(&mut out[..]), data.clone());
        futures::executor::block_on(future).unwrap();

        let (out_len, out_data) = unsigned_varint::decode::usize(&out).unwrap();
        assert_eq!(out_len, data.len());
        assert_eq!(&out_data[..out_len], &data[..]);
    }

    #[test]
    fn read_one_works() {
        let original_data = (0..rand::random::<usize>() % 10_000)
            .map(|_| rand::random::<u8>())
            .collect::<Vec<_>>();

        let mut len_buf = unsigned_varint::encode::usize_buffer();
        let len_buf = unsigned_varint::encode::usize(original_data.len(), &mut len_buf);

        let mut in_buffer = len_buf.to_vec();
        in_buffer.extend_from_slice(&original_data);

        let future = read_one_then(Cursor::new(in_buffer), 10_000, (), move |out, ()| -> Result<_, ReadOneError> {
            assert_eq!(out, original_data);
            Ok(())
        });

        futures::executor::block_on(future).unwrap();
    }

    #[test]
    fn read_one_zero_len() {
        let future = read_one_then(Cursor::new(vec![0]), 10_000, (), move |out, ()| -> Result<_, ReadOneError> {
            assert!(out.is_empty());
            Ok(())
        });

        futures::executor::block_on(future).unwrap();
    }

    #[test]
    fn read_checks_length() {
        let mut len_buf = unsigned_varint::encode::u64_buffer();
        let len_buf = unsigned_varint::encode::u64(5_000, &mut len_buf);

        let mut in_buffer = len_buf.to_vec();
        in_buffer.extend((0..5000).map(|_| 0));

        let future = read_one_then(Cursor::new(in_buffer), 100, (), move |_, ()| -> Result<_, ReadOneError> {
            Ok(())
        });

        match futures::executor::block_on(future) {
            Err(ReadOneError::TooLarge { .. }) => (),
            _ => panic!(),
        }
    }

    #[test]
    fn read_one_accepts_empty() {
        let future = read_one_then(Cursor::new([]), 10_000, (), move |out, ()| -> Result<_, ReadOneError> {
            assert!(out.is_empty());
            Ok(())
        });

        futures::executor::block_on(future).unwrap();
    }

    #[test]
    fn read_one_eof_before_len() {
        let future = read_one_then(Cursor::new([0x80]), 10_000, (), move |_, ()| -> Result<(), ReadOneError> {
            unreachable!()
        });

        match futures::executor::block_on(future) {
            Err(ReadOneError::Io(ref err)) if err.kind() == io::ErrorKind::UnexpectedEof => (),
            _ => panic!()
        }
    }
}
