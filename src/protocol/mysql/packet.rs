#![allow(unused)]

use crate::protocol::error::{ProtocolError, Result};
use bytes::BytesMut;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Compression algorithm used on a backend connection.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CompressAlgo {
    /// No compression (default).
    #[default]
    None,
    /// Standard MySQL zlib compression (`CLIENT_COMPRESS`).
    Zlib,
    /// MySQL 8.0.18+ zstd compression (`CLIENT_ZSTD_COMPRESSION_ALGORITHM`).
    Zstd,
}

/// PacketCodec handles the low-level MySQL protocol packet framing.
/// Each packet has a 4-byte header:
/// - 3 bytes: payload length (little-endian)
/// - 1 byte:  sequence id
///
/// When compression is enabled, an additional 7-byte compressed header wraps
/// the standard packet(s).
pub struct PacketCodec {
    sequence_id: u8,
    /// Write buffer: packets are accumulated here by `buffer_packet()`
    /// and flushed to the socket in a single `write_all` by `flush()`.
    write_buf: Vec<u8>,
    /// Read buffer: reused across `read_packet` calls to avoid
    /// repeated BytesMut allocation.
    read_buf: BytesMut,
    /// Whether compression is enabled for this connection.
    pub compressed: bool,
    /// Compressed packet sequence ID (separate from uncompressed seq).
    compressed_seq: u8,
    /// Compression algorithm when `compressed = true`.
    pub compression_algo: CompressAlgo,
}

impl PacketCodec {
    pub fn new() -> Self {
        Self {
            sequence_id: 0,
            write_buf: Vec::with_capacity(4096),
            read_buf: BytesMut::with_capacity(4096),
            compressed: false,
            compressed_seq: 0,
            compression_algo: CompressAlgo::None,
        }
    }

    /// Resets the sequence ID to 0. Important when starting a new command.
    pub fn reset_sequence(&mut self) {
        self.sequence_id = 0;
    }

    /// Returns the current sequence ID.
    pub fn sequence_id(&self) -> u8 {
        self.sequence_id
    }

    /// Sets the sequence ID explicitly (useful for proxy forwarding).
    pub fn set_sequence(&mut self, seq: u8) {
        self.sequence_id = seq;
    }

    /// Reads a single MySQL packet from the reader.
    pub async fn read_packet<R: AsyncReadExt + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<BytesMut> {
        let mut header = [0u8; 4];
        reader.read_exact(&mut header).await?;

        let length =
            (header[0] as usize) | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);
        let seq = header[3];

        if seq != self.sequence_id {
            return Err(ProtocolError::OutOfSequence {
                expected: self.sequence_id,
                got: seq,
            });
        }

        if length > 0xFFFFFF {
            return Err(ProtocolError::PacketTooLarge(length));
        }

        self.read_buf.clear();
        self.read_buf.resize(length, 0);
        reader.read_exact(&mut self.read_buf).await?;
        self.sequence_id = self.sequence_id.wrapping_add(1);

        // MySQL multi-packet reassembly: if a packet has length == 0xFFFFFF the
        // payload continues in the next packet(s).
        let mut last_length = length;
        while last_length == 0xFFFFFF {
            let mut next_header = [0u8; 4];
            reader.read_exact(&mut next_header).await?;
            let next_length = (next_header[0] as usize)
                | ((next_header[1] as usize) << 8)
                | ((next_header[2] as usize) << 16);
            let next_seq = next_header[3];
            if next_seq != self.sequence_id {
                return Err(ProtocolError::OutOfSequence {
                    expected: self.sequence_id,
                    got: next_seq,
                });
            }
            let old_len = self.read_buf.len();
            self.read_buf.resize(old_len + next_length, 0);
            reader.read_exact(&mut self.read_buf[old_len..]).await?;
            self.sequence_id = self.sequence_id.wrapping_add(1);
            last_length = next_length;
        }

        Ok(self.read_buf.split())
    }

    /// Reads raw bytes (header + payload) without parsing sequence IDs.
    /// Used by the proxy to forward packets transparently.
    pub async fn read_raw_packet<R: AsyncReadExt + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<Vec<u8>> {
        let mut header = [0u8; 4];
        reader.read_exact(&mut header).await?;

        let length =
            (header[0] as usize) | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);

        let mut raw = Vec::with_capacity(4 + length);
        raw.extend_from_slice(&header);
        raw.resize(4 + length, 0);
        reader.read_exact(&mut raw[4..]).await?;

        self.sequence_id = header[3].wrapping_add(1);
        Ok(raw)
    }

    /// Writes a payload as a MySQL packet to the writer.
    pub async fn write_packet<W: AsyncWriteExt + Unpin>(
        &mut self,
        writer: &mut W,
        payload: &[u8],
    ) -> Result<()> {
        let length = payload.len();
        if length > 0xFFFFFF {
            return Err(ProtocolError::PacketTooLarge(length));
        }

        let header = [
            (length & 0xFF) as u8,
            ((length >> 8) & 0xFF) as u8,
            ((length >> 16) & 0xFF) as u8,
            self.sequence_id,
        ];

        let mut combined = Vec::with_capacity(4 + length);
        combined.extend_from_slice(&header);
        combined.extend_from_slice(payload);
        writer.write_all(&combined).await?;

        self.sequence_id = self.sequence_id.wrapping_add(1);
        Ok(())
    }

    /// Writes raw bytes directly to the writer without framing.
    /// Used by the proxy to forward already-framed packets.
    pub async fn write_raw<W: AsyncWriteExt + Unpin>(
        &mut self,
        writer: &mut W,
        raw: &[u8],
    ) -> Result<()> {
        writer.write_all(raw).await?;
        Ok(())
    }

    /// Queue a packet into the internal write buffer.
    pub fn buffer_packet(&mut self, payload: &[u8]) -> Result<()> {
        let length = payload.len();
        if length > 0xFFFFFF {
            return Err(ProtocolError::PacketTooLarge(length));
        }

        self.write_buf.reserve(4 + length);
        self.write_buf.push((length & 0xFF) as u8);
        self.write_buf.push(((length >> 8) & 0xFF) as u8);
        self.write_buf.push(((length >> 16) & 0xFF) as u8);
        self.write_buf.push(self.sequence_id);
        self.write_buf.extend_from_slice(payload);

        self.sequence_id = self.sequence_id.wrapping_add(1);
        Ok(())
    }

    /// Flush all buffered packets to the writer in a single `write_all`.
    pub async fn flush<W: AsyncWriteExt + Unpin>(&mut self, writer: &mut W) -> Result<()> {
        if !self.write_buf.is_empty() {
            writer.write_all(&self.write_buf).await?;
            self.write_buf.clear();
        }
        Ok(())
    }

    /// Append pre-encoded raw bytes to the write buffer.
    #[inline]
    pub fn buffer_raw(&mut self, raw: &[u8]) {
        self.write_buf.extend_from_slice(raw);
    }

    /// Advance the internal sequence id by `n` packets.
    #[inline]
    pub fn advance_seq(&mut self, n: u8) {
        self.sequence_id = self.sequence_id.wrapping_add(n);
    }

    /// Enable compression for this codec (zlib or zstd).
    pub fn enable_compression(&mut self, algo: CompressAlgo) {
        self.compressed = true;
        self.compressed_seq = 0;
        self.compression_algo = algo;
    }

    /// Flush all buffered packets, optionally compressing them (zlib or zstd).
    pub async fn flush_maybe_compressed<W: AsyncWriteExt + Unpin>(
        &mut self,
        writer: &mut W,
    ) -> Result<()> {
        if !self.compressed || self.write_buf.is_empty() {
            return self.flush(writer).await;
        }

        let payload = std::mem::take(&mut self.write_buf);
        let uncompressed_len = payload.len();

        const MIN_COMPRESS_THRESHOLD: usize = 50;
        if uncompressed_len > MIN_COMPRESS_THRESHOLD {
            let compressed = match self.compression_algo {
                CompressAlgo::Zstd => zstd::bulk::compress(&payload, 3)
                    .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))?,
                _ => {
                    // Zlib (also the default fallback)
                    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                    encoder.write_all(&payload).map_err(ProtocolError::Io)?;
                    encoder.finish().map_err(ProtocolError::Io)?
                }
            };

            let comp_len = compressed.len();
            let mut header = [0u8; 7];
            header[0] = (comp_len & 0xFF) as u8;
            header[1] = ((comp_len >> 8) & 0xFF) as u8;
            header[2] = ((comp_len >> 16) & 0xFF) as u8;
            header[3] = self.compressed_seq;
            header[4] = (uncompressed_len & 0xFF) as u8;
            header[5] = ((uncompressed_len >> 8) & 0xFF) as u8;
            header[6] = ((uncompressed_len >> 16) & 0xFF) as u8;

            self.compressed_seq = self.compressed_seq.wrapping_add(1);
            writer.write_all(&header).await?;
            writer.write_all(&compressed).await?;
        } else {
            // Too small to compress — send raw.
            let mut header = [0u8; 7];
            header[0] = (uncompressed_len & 0xFF) as u8;
            header[1] = ((uncompressed_len >> 8) & 0xFF) as u8;
            header[2] = ((uncompressed_len >> 16) & 0xFF) as u8;
            header[3] = self.compressed_seq;
            // header[4..7] = 0 (uncompressed_len = 0 means "not compressed")

            self.compressed_seq = self.compressed_seq.wrapping_add(1);
            writer.write_all(&header).await?;
            writer.write_all(&payload).await?;
        }
        Ok(())
    }

    /// Read a packet, handling decompression if compression is enabled.
    pub async fn read_packet_maybe_compressed<R: AsyncReadExt + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<BytesMut> {
        if !self.compressed {
            return self.read_packet(reader).await;
        }

        let mut header = [0u8; 7];
        reader.read_exact(&mut header).await?;

        let compressed_length =
            (header[0] as usize) | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);
        let _comp_seq = header[3];
        let uncompressed_length =
            (header[4] as usize) | ((header[5] as usize) << 8) | ((header[6] as usize) << 16);

        let mut compressed_data = vec![0u8; compressed_length];
        reader.read_exact(&mut compressed_data).await?;

        let decompressed = if uncompressed_length > 0 {
            let mut decoder = ZlibDecoder::new(&compressed_data[..]);
            let mut buf = vec![0u8; uncompressed_length];
            decoder.read_exact(&mut buf).map_err(ProtocolError::Io)?;
            buf
        } else {
            compressed_data
        };

        if decompressed.len() < 4 {
            return Err(ProtocolError::PacketTooLarge(0));
        }
        let length = (decompressed[0] as usize)
            | ((decompressed[1] as usize) << 8)
            | ((decompressed[2] as usize) << 16);
        let _seq = decompressed[3];
        self.sequence_id = _seq.wrapping_add(1);

        let payload = &decompressed[4..4 + length.min(decompressed.len() - 4)];
        let mut result = BytesMut::with_capacity(payload.len());
        result.extend_from_slice(payload);
        Ok(result)
    }
}

// ─── MysqlCompressedReader ────────────────────────────────────────────────────

/// An `AsyncRead` adapter that transparently decompresses MySQL compressed-protocol
/// framing so that callers (e.g. `collect_response_tracked`) can read standard
/// 4-byte-framed MySQL packets without knowing about the outer compression layer.
///
/// MySQL compressed packet format (both CLIENT_COMPRESS / zlib and
/// CLIENT_ZSTD_COMPRESSION_ALGORITHM / zstd):
/// ```text
/// [3 bytes] compressed payload length  (0 = payload not compressed)
/// [1 byte]  compressed sequence id
/// [3 bytes] uncompressed payload length (0 = payload is already raw)
/// [N bytes] payload
/// ```
/// When `uncompressed_payload_length > 0` the payload is compressed; otherwise
/// it is sent raw (packets too small to bother compressing).
pub struct MysqlCompressedReader {
    inner: Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>,
    /// Decompressed data waiting to be returned to the caller.
    buf: Vec<u8>,
    buf_pos: usize,
    /// Sub-state machine.
    phase: CompReadPhase,
    /// 7-byte compressed packet header (filled incrementally).
    hdr: [u8; 7],
    hdr_filled: usize,
    /// Compressed payload bytes (filled incrementally).
    comp: Vec<u8>,
    comp_filled: usize,
    algo: CompressAlgo,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CompReadPhase {
    DrainBuf,
    ReadHeader,
    ReadPayload,
}

impl MysqlCompressedReader {
    pub fn new(
        inner: Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>,
        algo: CompressAlgo,
    ) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            buf_pos: 0,
            phase: CompReadPhase::DrainBuf,
            hdr: [0u8; 7],
            hdr_filled: 0,
            comp: Vec::new(),
            comp_filled: 0,
            algo,
        }
    }
}

impl tokio::io::AsyncRead for MysqlCompressedReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        dst: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::task::Poll;
        let this = self.get_mut();

        loop {
            match this.phase {
                CompReadPhase::DrainBuf => {
                    let avail = this.buf.len().saturating_sub(this.buf_pos);
                    if avail > 0 {
                        let to_copy = avail.min(dst.remaining());
                        dst.put_slice(&this.buf[this.buf_pos..this.buf_pos + to_copy]);
                        this.buf_pos += to_copy;
                        return Poll::Ready(Ok(()));
                    }
                    // Buffer exhausted — start reading next compressed packet.
                    this.buf.clear();
                    this.buf_pos = 0;
                    this.hdr_filled = 0;
                    this.phase = CompReadPhase::ReadHeader;
                }

                CompReadPhase::ReadHeader => {
                    // Keep reading until we have all 7 header bytes.
                    while this.hdr_filled < 7 {
                        let mut rb = tokio::io::ReadBuf::new(&mut this.hdr[this.hdr_filled..]);
                        match std::pin::Pin::new(&mut *this.inner).poll_read(cx, &mut rb) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                            Poll::Ready(Ok(())) => {
                                let n = rb.filled().len();
                                if n == 0 {
                                    // EOF on the inner stream.
                                    return Poll::Ready(Ok(()));
                                }
                                this.hdr_filled += n;
                            }
                        }
                    }
                    // Header complete — extract compressed length and allocate payload buf.
                    let comp_len = (this.hdr[0] as usize)
                        | ((this.hdr[1] as usize) << 8)
                        | ((this.hdr[2] as usize) << 16);
                    this.comp = vec![0u8; comp_len];
                    this.comp_filled = 0;
                    this.phase = CompReadPhase::ReadPayload;
                }

                CompReadPhase::ReadPayload => {
                    // Keep reading until we have all compressed payload bytes.
                    while this.comp_filled < this.comp.len() {
                        let mut rb = tokio::io::ReadBuf::new(&mut this.comp[this.comp_filled..]);
                        match std::pin::Pin::new(&mut *this.inner).poll_read(cx, &mut rb) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                            Poll::Ready(Ok(())) => {
                                let n = rb.filled().len();
                                if n == 0 && this.comp_filled < this.comp.len() {
                                    return Poll::Ready(Ok(())); // EOF mid-payload
                                }
                                this.comp_filled += n;
                            }
                        }
                    }
                    // Payload complete — decompress.
                    let uncomp_len = (this.hdr[4] as usize)
                        | ((this.hdr[5] as usize) << 8)
                        | ((this.hdr[6] as usize) << 16);

                    let decompressed = if uncomp_len > 0 {
                        match this.algo {
                            CompressAlgo::Zstd => zstd::bulk::decompress(&this.comp, uncomp_len)
                                .map_err(|e| std::io::Error::other(e.to_string()))?,
                            _ => {
                                // Zlib
                                let mut out = vec![0u8; uncomp_len];
                                let mut decoder = ZlibDecoder::new(&this.comp[..]);
                                decoder
                                    .read_exact(&mut out)
                                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                                out
                            }
                        }
                    } else {
                        // Payload was not compressed — use raw bytes.
                        std::mem::take(&mut this.comp)
                    };

                    this.buf = decompressed;
                    this.buf_pos = 0;
                    this.phase = CompReadPhase::DrainBuf;
                }
            }
        }
    }
}
