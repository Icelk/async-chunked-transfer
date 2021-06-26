// Copyright 2015 The tiny-http Contributors
// Copyright 2015 The rust-chunked-transfer Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::error::Error;
use std::fmt;
use std::io::Error as IoError;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

macro_rules! ok_ready {
    ($poll: expr) => {
        match $poll {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(_)) => return Poll::Ready(Err(IoError::new(ErrorKind::InvalidInput, DecoderError))),
            Poll::Ready(Ok(v)) => v,
        }
    };
}

/// Reads HTTP chunks and sends back real data.
///
/// # Example
///
/// ```
/// use async_chunked_transfer::Decoder;
/// use tokio::io::AsyncReadExt;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn test() {
/// let encoded = b"3\r\nhel\r\nb\r\nlo world!!!\r\n0\r\n\r\n";
/// let mut decoded = String::new();
///
/// let mut decoder = Decoder::new(encoded as &[u8]);
/// decoder.read_to_string(&mut decoded).await;
///
/// assert_eq!(decoded, "hello world!!!");
/// # }
/// ```
pub struct Decoder<R> {
    // where the chunks come from
    source: R,

    // remaining size of the chunk being read
    // none if we are not in a chunk
    remaining_chunks_size: Option<usize>,
}

impl<R> Decoder<R>
where
    R: AsyncRead + Unpin,
{
    pub fn new(source: R) -> Decoder<R> {
        Decoder {
            source,
            remaining_chunks_size: None,
        }
    }

    /// Returns the remaining bytes left in the chunk being read.
    pub fn remaining_chunks_size(&self) -> Option<usize> {
        self.remaining_chunks_size
    }

    /// Unwraps the Decoder into its inner `Read` source.
    pub fn into_inner(self) -> R {
        self.source
    }

    fn read_byte(&mut self, cx: &mut Context) -> Poll<IoResult<u8>> {
        let mut buffer = [0; 1];
        let mut read_buf = ReadBuf::new(&mut buffer);
        ok_ready!(Pin::new(&mut self.source).poll_read(cx, &mut read_buf));
        if read_buf.filled().len() != 1 {
            return Poll::Ready(Err(IoError::new(ErrorKind::InvalidInput, DecoderError)));
        }
        Poll::Ready(Ok(buffer[0]))
    }

    fn read_chunk_size(&mut self, cx: &mut Context) -> Poll<IoResult<usize>> {
        let mut chunk_size_bytes = Vec::new();
        let mut has_ext = false;

        loop {
            let byte = ok_ready!(self.read_byte(cx));

            if byte == b'\r' {
                break;
            }

            if byte == b';' {
                has_ext = true;
                break;
            }

            chunk_size_bytes.push(byte);
        }

        // Ignore extensions for now
        if has_ext {
            loop {
                let byte = ok_ready!(self.read_byte(cx));
                if byte == b'\r' {
                    break;
                }
            }
        }

        ok_ready!(self.read_line_feed(cx));

        let chunk_size = String::from_utf8(chunk_size_bytes)
            .ok()
            .and_then(|c| usize::from_str_radix(c.trim(), 16).ok())
            .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, DecoderError))?;

        Poll::Ready(Ok(chunk_size))
    }

    fn read_carriage_return(&mut self, cx: &mut Context) -> Poll<IoResult<()>> {
        Poll::Ready(match self.read_byte(cx) {
            Poll::Ready(Ok(b'\r')) => Ok(()),
            Poll::Ready(_) => Err(IoError::new(ErrorKind::InvalidInput, DecoderError)),
            Poll::Pending => return Poll::Pending,
        })
    }

    fn read_line_feed(&mut self, cx: &mut Context) -> Poll<IoResult<()>> {
        Poll::Ready(match self.read_byte(cx) {
            Poll::Ready(Ok(b'\n')) => Ok(()),
            Poll::Ready(_) => Err(IoError::new(ErrorKind::InvalidInput, DecoderError)),
            Poll::Pending => return Poll::Pending,
        })
    }
}

impl<R> AsyncRead for Decoder<R>
where
    R: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<IoResult<()>> {
        let remaining_chunks_size = match self.remaining_chunks_size {
            Some(c) => c,
            None => {
                // first possibility: we are not in a chunk, so we'll attempt to determine
                // the chunks size
                let chunk_size = ok_ready!(self.read_chunk_size(cx));

                // if the chunk size is 0, we are at EOF
                if chunk_size == 0 {
                    ok_ready!(self.read_carriage_return(cx));
                    ok_ready!(self.read_line_feed(cx));
                    return Poll::Ready(Ok(()));
                }

                chunk_size
            }
        };

        // second possibility: we continue reading from a chunk
        if buf.remaining() < remaining_chunks_size {
            let before_read = buf.filled().len();
            ok_ready!(Pin::new(&mut self.source).poll_read(cx, buf));
            let read = buf.filled().len() - before_read;
            self.remaining_chunks_size = Some(remaining_chunks_size - read);
            return Poll::Ready(Ok(()));
        }

        // third possibility: the read request goes further than the current chunk
        // we simply read until the end of the chunk and return
        assert!(buf.remaining() >= remaining_chunks_size);

        let read = unsafe {
            buf.assume_init(remaining_chunks_size);
            let mut unfilled = ReadBuf::new(
                &mut std::mem::transmute::<_, &mut [u8]>(buf.unfilled_mut())
                    [..remaining_chunks_size],
            );
            ok_ready!(Pin::new(&mut self.source).poll_read(cx, &mut unfilled));
            unfilled.filled().len()
        };

        self.remaining_chunks_size = if read == remaining_chunks_size {
            ok_ready!(self.read_carriage_return(cx));
            ok_ready!(self.read_line_feed(cx));
            None
        } else {
            Some(remaining_chunks_size - read)
        };

        buf.advance(read);

        Poll::Ready(Ok(()))
    }
}

#[derive(Debug, Copy, Clone)]
struct DecoderError;

impl fmt::Display for DecoderError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(fmt, "Error while decoding chunks")
    }
}

impl Error for DecoderError {
    fn description(&self) -> &str {
        "Error while decoding chunks"
    }
}

#[cfg(test)]
mod test {
    use super::{Context, Decoder, Pin, Poll};
    use std::io;
    use tokio::io::AsyncReadExt;

    /// This unit test is taken from from Hyper
    /// https://github.com/hyperium/hyper
    /// Copyright (c) 2014 Sean McArthur
    ///
    /// Modified to fit Tokio by Icelk.
    #[tokio::test]
    async fn test_read_chunk_size() {
        async fn read(s: &str, expected: usize) {
            let mut decoded = Decoder::new(s.as_bytes());
            crate::poll_fn(|cx| {
                let actual = match decoded.read_chunk_size(cx) {
                    Poll::Pending => panic!(),
                    Poll::Ready(r) => r.unwrap(),
                };
                assert_eq!(expected, actual);
                Poll::Ready(())
            })
            .await;
        }

        async fn read_err(s: &str) {
            let mut decoded = Decoder::new(s.as_bytes());
            crate::poll_fn(|cx| {
                let err_kind = match decoded.read_chunk_size(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(r) => r.unwrap_err().kind(),
                };
                assert_eq!(err_kind, io::ErrorKind::InvalidInput);
                Poll::Ready(())
            })
            .await;
        }

        read("1\r\n", 1).await;
        read("01\r\n", 1).await;
        read("0\r\n", 0).await;
        read("00\r\n", 0).await;
        read("A\r\n", 10).await;
        read("a\r\n", 10).await;
        read("Ff\r\n", 255).await;
        read("Ff   \r\n", 255).await;
        // Missing LF or CRLF
        read_err("F\rF").await;
        read_err("F").await;
        // Invalid hex digit
        read_err("X\r\n").await;
        read_err("1X\r\n").await;
        read_err("-\r\n").await;
        read_err("-1\r\n").await;
        // Acceptable (if not fully valid) extensions do not influence the size
        read("1;extension\r\n", 1).await;
        read("a;ext name=value\r\n", 10).await;
        read("1;extension;extension2\r\n", 1).await;
        read("1;;;  ;\r\n", 1).await;
        read("2; extension...\r\n", 2).await;
        read("3   ; extension=123\r\n", 3).await;
        read("3   ;\r\n", 3).await;
        read("3   ;   \r\n", 3).await;
        // Invalid extensions cause an error
        read_err("1 invalid extension\r\n").await;
        read_err("1 A\r\n").await;
        read_err("1;no CRLF").await;
    }

    #[tokio::test]
    async fn test_valid_chunk_decode() {
        let source = io::Cursor::new(
            "3\r\nhel\r\nb\r\nlo world!!!\r\n0\r\n\r\n"
                .to_string()
                .into_bytes(),
        );
        let mut decoded = Decoder::new(source);

        let mut string = String::new();
        decoded.read_to_string(&mut string).await.unwrap();

        assert_eq!(string, "hello world!!!");
    }

    #[tokio::test]
    async fn test_decode_zero_length() {
        let mut decoder = Decoder::new(b"0\r\n\r\n" as &[u8]);

        let mut decoded = String::new();
        decoder.read_to_string(&mut decoded).await.unwrap();

        assert_eq!(decoded, "");
    }

    #[tokio::test]
    async fn test_decode_invalid_chunk_length() {
        let mut decoder = Decoder::new(b"m\r\n\r\n" as &[u8]);

        let mut decoded = String::new();
        assert!(decoder.read_to_string(&mut decoded).await.is_err());
    }

    #[tokio::test]
    async fn invalid_input1() {
        let source = io::Cursor::new(
            "2\r\nhel\r\nb\r\nlo world!!!\r\n0\r\n"
                .to_string()
                .into_bytes(),
        );
        let mut decoded = Decoder::new(source);

        let mut string = String::new();
        assert!(decoded.read_to_string(&mut string).await.is_err());
    }

    #[tokio::test]
    async fn invalid_input2() {
        let source = io::Cursor::new(
            "3\rhel\r\nb\r\nlo world!!!\r\n0\r\n"
                .to_string()
                .into_bytes(),
        );
        let mut decoded = Decoder::new(source);

        let mut string = String::new();
        assert!(decoded.read_to_string(&mut string).await.is_err());
    }
}
