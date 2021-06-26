# async-chunked-transfer

> Async fork of [chunked-transfer](https://github.com/frewsxcv/rust-chunked-transfer).

**Note:** The only difference between this and `chunked_transfer` is
that you MUST call `Encoder::finish` before dropping the encoder.

[Documentation](https://docs.rs/async_chunked_transfer/)

Encoder and decoder for HTTP chunked transfer coding. For more information about chunked transfer encoding:

* [RFC 7230 § 4.1](https://tools.ietf.org/html/rfc7230#section-4.1)
* [RFC 2616 § 3.6.1](http://www.w3.org/Protocols/rfc2616/rfc2616-sec3.html#sec3.6.1) (deprecated)
* [Wikipedia: Chunked transfer encoding](https://en.wikipedia.org/wiki/Chunked_transfer_encoding)

## Example

### Decoding

```rust
use async_chunked_transfer::Decoder;
use tokio::io::AsyncReadExt;

let encoded = b"3\r\nhel\r\nb\r\nlo world!!!\r\n0\r\n\r\n";
let mut decoded = String::new();

let mut decoder = Decoder::new(encoded as &[u8]);
decoder.read_to_string(&mut decoded).await;

assert_eq!(decoded, "hello world!!!");
```

### Encoding

```rust
use chunked_transfer::Encoder;
use tokio::io::AsyncWriteExt;

let mut decoded = "hello world";
let mut encoded: Vec<u8> = vec![];

{
    let mut encoder = Encoder::with_chunks_size(&mut encoded, 5);
    encoder.write_all(decoded.as_bytes()).await;
    encoder.finish().await;
}

assert_eq!(encoded, b"5\r\nhello\r\n5\r\n worl\r\n1\r\nd\r\n0\r\n\r\n");
```

## Authors

* [Icelk](https://github.com/icelk)
* [tomaka](https://github.com/tomaka)
* [frewsxcv](https://github.com/frewsxcv)
