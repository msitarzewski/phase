// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shared bounded HTTP response reading and Server-Sent Events (SSE) framing
//! utilities reused by local inference subprocess backends (`worker_llama`,
//! `worker_mlx`).

use bytes::{Bytes, BytesMut};
use futures::StreamExt;

/// Maximum byte size of a single SSE event frame accepted from a loopback
/// backend subprocess.
pub(crate) const MAX_SSE_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BackendBodyReadError {
    TooLarge { limit: usize },
    Transport(String),
}

impl std::fmt::Display for BackendBodyReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { limit } => {
                write!(
                    formatter,
                    "backend response body exceeded {limit} byte limit"
                )
            }
            Self::Transport(error) => write!(formatter, "backend response read failed: {error}"),
        }
    }
}

pub(crate) fn extend_limited_body(
    body: &mut BytesMut,
    chunk: &[u8],
    limit: usize,
) -> Result<(), BackendBodyReadError> {
    if chunk.len() > limit.saturating_sub(body.len()) {
        return Err(BackendBodyReadError::TooLarge { limit });
    }
    body.extend_from_slice(chunk);
    Ok(())
}

pub(crate) async fn read_limited_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, BackendBodyReadError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(BackendBodyReadError::TooLarge { limit });
    }
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limit);
    let mut body = BytesMut::with_capacity(initial_capacity);
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| BackendBodyReadError::Transport(error.to_string()))?;
        extend_limited_body(&mut body, &chunk, limit)?;
    }
    Ok(body.freeze())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SseFrameTooLarge;

impl std::fmt::Display for SseFrameTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "backend SSE frame exceeded {MAX_SSE_FRAME_BYTES} byte limit"
        )
    }
}

/// Find the earliest SSE event separator (`\n\n` or `\r\n\r\n`), returning
/// `(start_offset, delimiter_len)`.
pub(crate) fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
    }
}

/// Refuse an oversized first SSE frame before copying a delimiter-free chunk
/// into the accumulation buffer — scanning across the `(buffer, chunk)` join
/// on a 6-byte stack window without allocating a combined copy.
pub(crate) fn append_sse_chunk(
    buffer: &mut BytesMut,
    chunk: &[u8],
) -> Result<(), SseFrameTooLarge> {
    let tail_len = buffer.len().min(3);
    let head_len = chunk.len().min(3);
    let mut seam = [0u8; 6];
    let seam_len = tail_len + head_len;
    seam[..tail_len].copy_from_slice(&buffer[buffer.len() - tail_len..]);
    seam[tail_len..seam_len].copy_from_slice(&chunk[..head_len]);

    let first_boundary = find_sse_boundary(buffer)
        .map(|(position, _)| position)
        .or_else(|| {
            find_sse_boundary(&seam[..seam_len])
                .map(|(position, _)| (buffer.len() - tail_len).saturating_add(position))
        })
        .or_else(|| {
            find_sse_boundary(chunk).map(|(position, _)| buffer.len().saturating_add(position))
        });

    let accumulated = buffer.len().saturating_add(chunk.len());
    if first_boundary
        .map(|position| position > MAX_SSE_FRAME_BYTES)
        .unwrap_or(accumulated > MAX_SSE_FRAME_BYTES)
    {
        return Err(SseFrameTooLarge);
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

pub(crate) fn take_next_sse_frame(
    buffer: &mut BytesMut,
) -> Result<Option<Bytes>, SseFrameTooLarge> {
    match find_sse_boundary(buffer) {
        Some((position, _)) if position > MAX_SSE_FRAME_BYTES => Err(SseFrameTooLarge),
        Some((position, delimiter_len)) => {
            let mut framed = buffer.split_to(position + delimiter_len);
            framed.truncate(position);
            Ok(Some(framed.freeze()))
        }
        None if buffer.len() > MAX_SSE_FRAME_BYTES => Err(SseFrameTooLarge),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_sse_boundary_handles_lf_and_crlf() {
        assert_eq!(find_sse_boundary(b"hello\n\nworld\n\n"), Some((5, 2)));
        assert_eq!(find_sse_boundary(b"hello\r\n\r\nworld"), Some((5, 4)));
        assert_eq!(find_sse_boundary(b"a\n\nb\r\n\r\n"), Some((1, 2)));
        assert_eq!(find_sse_boundary(b"a\r\n\r\nb\n\n"), Some((1, 4)));
    }

    #[test]
    fn append_sse_chunk_detects_cross_chunk_seam_at_exact_limit() {
        // Test `\n` + `\n` across chunk seam right at MAX_SSE_FRAME_BYTES.
        let mut lf_buf = BytesMut::from(vec![b'a'; MAX_SSE_FRAME_BYTES].as_slice());
        lf_buf.extend_from_slice(b"\n");
        // Buffer already exceeds MAX_SSE_FRAME_BYTES by 1 without boundary: rejected if no boundary before limit.
        assert_eq!(append_sse_chunk(&mut lf_buf, b"\n"), Ok(()));
        let frame = take_next_sse_frame(&mut lf_buf).unwrap().unwrap();
        assert_eq!(frame.len(), MAX_SSE_FRAME_BYTES);

        // Test `\r\n` + `\r\n` across chunk seam right at MAX_SSE_FRAME_BYTES.
        let mut crlf_buf = BytesMut::from(vec![b'b'; MAX_SSE_FRAME_BYTES].as_slice());
        crlf_buf.extend_from_slice(b"\r\n");
        assert_eq!(append_sse_chunk(&mut crlf_buf, b"\r\n"), Ok(()));
        let frame = take_next_sse_frame(&mut crlf_buf).unwrap().unwrap();
        assert_eq!(frame.len(), MAX_SSE_FRAME_BYTES);
    }

    #[test]
    fn append_sse_chunk_rejects_oversized_unterminated_frame() {
        let mut buffer = BytesMut::from(vec![b'x'; MAX_SSE_FRAME_BYTES].as_slice());
        assert_eq!(append_sse_chunk(&mut buffer, b"y"), Err(SseFrameTooLarge));
        assert_eq!(buffer.len(), MAX_SSE_FRAME_BYTES);
    }
}
