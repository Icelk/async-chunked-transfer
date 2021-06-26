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

use std::io::Result as IoResult;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncWrite;

macro_rules! ok_ready {
    ($poll: expr) => {
        match $poll {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Ready(Ok(v)) => v,
        }
    };
}

/// Splits the incoming data into HTTP chunks.
///
/// **NOTE**: Since this is async, we cannot finish the writer in the [`Drop`] implementation.
/// Therefore, it's crucial for you to call [`Encoder::finish`] before dropping this.
///
/// # Example
///
/// ```
/// use async_chunked_transfer::Encoder;
/// use tokio::io::AsyncWriteExt;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn test () {
/// let mut decoded = "hello world";
/// let mut encoded: Vec<u8> = vec![];
///
/// {
///     let mut encoder = Encoder::with_chunks_size(&mut encoded, 5);
///     encoder.write_all(decoded.as_bytes()).await;
///     encoder.finish().await;
/// }
///
/// assert_eq!(encoded, b"5\r\nhello\r\n5\r\n worl\r\n1\r\nd\r\n0\r\n\r\n");
/// # }
/// ```
pub struct Encoder<W>
where
    W: AsyncWrite + Unpin,
{
    // where to send the result
    output: W,

    // size of each chunk
    chunks_size: usize,

    // data waiting to be sent is stored here
    // This will always be at least 6 bytes long. The first 6 bytes
    // are reserved for the chunk size and \r\n.
    buffer: Vec<u8>,

    // Flushes the internal buffer after each write. This might be useful
    // if data should be sent immediately to downstream consumers
    flush_after_write: bool,

    // The offset we are writing from in the buffer.
    // Will be `Some` if we are currently writing a chunk.
    offset: Option<usize>,
}

const MAX_CHUNK_SIZE: usize = std::u32::MAX as usize;
// This accounts for four hex digits (enough to hold a u32) plus two bytes
// for the \r\n
const MAX_HEADER_SIZE: usize = 6;

impl<W> Encoder<W>
where
    W: AsyncWrite + Unpin,
{
    pub fn new(output: W) -> Encoder<W> {
        Encoder::with_chunks_size(output, 8192)
    }

    pub fn with_chunks_size(output: W, chunks: usize) -> Encoder<W> {
        let chunks_size = chunks.min(MAX_CHUNK_SIZE);
        let mut encoder = Encoder {
            output,
            chunks_size,
            buffer: vec![0; MAX_HEADER_SIZE],
            flush_after_write: false,
            offset: None,
        };
        encoder.reset_buffer();
        encoder
    }

    pub fn with_flush_after_write(output: W) -> Encoder<W> {
        let mut encoder = Encoder {
            output,
            chunks_size: 8192,
            buffer: vec![0; MAX_HEADER_SIZE],
            flush_after_write: true,
            offset: None,
        };
        encoder.reset_buffer();
        encoder
    }

    fn reset_buffer(&mut self) {
        // Reset buffer, still leaving space for the chunk size. That space
        // will be populated once we know the size of the chunk.
        self.buffer.truncate(MAX_HEADER_SIZE);

        self.offset = None;
    }

    fn is_buffer_empty(&self) -> bool {
        self.buffer.len() == MAX_HEADER_SIZE
    }

    fn buffer_len(&self) -> usize {
        self.buffer.len() - MAX_HEADER_SIZE
    }

    fn send(&mut self, cx: &mut Context) -> Poll<IoResult<()>> {
        // First possibility; we have data in our buffer to write
        if let Some(mut offset) = self.offset {
            loop {
                let wrote =
                    ok_ready!(Pin::new(&mut self.output).poll_write(cx, &self.buffer[offset..]));
                offset += wrote;

                self.offset = Some(offset);

                if offset >= self.buffer.len() {
                    self.reset_buffer();
                    break;
                }
            }
            // self.send(cx)
            Poll::Ready(Ok(()))
            // Second possibility; we create a new chunk.
        } else {
            // Never send an empty buffer, because that would be interpreted
            // as the end of the stream, which we indicate explicitly on drop.
            if self.is_buffer_empty() {
                return Poll::Ready(Ok(()));
            }
            // Prepend the length and \r\n to the buffer.
            let prelude = format!("{:x}\r\n", self.buffer_len());
            let prelude = prelude.as_bytes();

            // This should never happen because MAX_CHUNK_SIZE of u32::MAX
            // can always be encoded in 4 hex bytes.
            assert!(
                prelude.len() <= MAX_HEADER_SIZE,
                "invariant failed: prelude longer than MAX_HEADER_SIZE"
            );

            // Copy the prelude into the buffer. For small chunks, this won't necessarily
            // take up all the space that was reserved for the prelude.
            let offset = MAX_HEADER_SIZE - prelude.len();
            self.buffer[offset..MAX_HEADER_SIZE].clone_from_slice(&prelude);

            // Append the chunk-finishing \r\n to the buffer.
            self.buffer.extend_from_slice(b"\r\n");

            self.offset = Some(offset);

            self.send(cx)
        }
    }
    fn poll_finish(&mut self, cx: &mut Context) -> Poll<IoResult<()>> {
        ok_ready!(self.send(cx));
        Pin::new(&mut self.output)
            .poll_write(cx, b"0\r\n\r\n" as &[u8])
            .map(|r| r.map(|_| ()))
    }
    /// Finishes the chunked-transfer with a 0-size chunk.
    ///
    /// This should only be called when ALL data is written to this.
    /// To push any data in the buffer, use [`tokio::io::AsyncWriteExt::flush`].
    pub async fn finish (mut self) -> IoResult<()> {
        crate::poll_fn(|cx| self.poll_finish(cx)).await
    }

    fn priv_poll_write(mut self: Pin<&mut Self>, cx: &mut Context, data: &[u8], data_written: usize) -> Poll<IoResult<usize>> {
        let remaining_buffer_space = self.chunks_size - self.buffer_len();
        let bytes_to_buffer = std::cmp::min(remaining_buffer_space, data.len());
        self.buffer.extend_from_slice(&data[0..bytes_to_buffer]);
        let more_to_write: bool = bytes_to_buffer < data.len();
        if self.flush_after_write || more_to_write {
            ok_ready!(self.send(cx));
        }

        // If we didn't write the whole thing, keep working on it.
        if more_to_write {
            return self.priv_poll_write(cx, &data[bytes_to_buffer..], bytes_to_buffer + data_written);
        }
        Poll::Ready(Ok(bytes_to_buffer + data_written))
    }
}

impl<W> AsyncWrite for Encoder<W>
where
    W: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<IoResult<usize>> {
        self.priv_poll_write(cx, data, 0)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        self.send(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        ok_ready!(self.poll_finish(cx));
        Pin::new(&mut self.output).poll_shutdown(cx)
    }
}

impl<W> Drop for Encoder<W>
where
    W: AsyncWrite + Unpin,
{
    fn drop(&mut self) {
        if !self.is_buffer_empty() {
            eprintln!(
                "Dropping non-empty Chunked-Transfer encoder. This will cause invalid output."
            )
        }
    }
}

#[cfg(test)]
mod test {
    use super::Encoder;
    use std::io::Cursor;
    use std::io::Write;
    use std::str::from_utf8;
    use tokio::io::copy;

    #[tokio::test]
    async fn test() {
        let mut source = Cursor::new("hello world".to_string().into_bytes());
        let mut dest: Vec<u8> = vec![];

        {
            let mut encoder = Encoder::with_chunks_size(dest.by_ref(), 5);
            copy(&mut source, &mut encoder).await.unwrap();
            // Tokio's copy function calls flush, so this isn't guaranteed.
            // assert!(!encoder.is_buffer_empty());
            encoder.finish().await.unwrap();
        }

        let output = from_utf8(&dest).unwrap();

        assert_eq!(output, "5\r\nhello\r\n5\r\n worl\r\n1\r\nd\r\n0\r\n\r\n");
    }
    #[tokio::test]
    async fn flush_after_write() {
        let mut source = Cursor::new("hello world".to_string().into_bytes());
        let mut dest: Vec<u8> = vec![];

        {
            let mut encoder = Encoder::with_flush_after_write(dest.by_ref());
            copy(&mut source, &mut encoder).await.unwrap();
            // The internal buffer has been flushed.
            assert!(encoder.is_buffer_empty());
            encoder.finish().await.unwrap();
        }

        let output = from_utf8(&dest).unwrap();

        assert_eq!(output, "b\r\nhello world\r\n0\r\n\r\n");
    }
}
