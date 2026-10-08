//! JSON Lines framing.
//!
//! One compact JSON object per `\n`-terminated line. `serde_json` escapes
//! newlines inside strings, so the delimiter is unambiguous, and a frame can be
//! read with `socat`/`nc -U` during debugging. Frames are capped so a
//! misbehaving peer cannot exhaust memory.

use std::io::{BufRead, Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::IpcError;

/// Upper bound for a single frame. A finalized sale with hundreds of lines is
/// still far below this.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Serialize a frame and flush it as one line.
pub fn write_frame<W, T>(writer: &mut W, frame: &T) -> Result<(), IpcError>
where
    W: Write,
    T: Serialize + ?Sized,
{
    let mut encoded =
        serde_json::to_vec(frame).map_err(|error| IpcError::Codec(error.to_string()))?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            limit: MAX_FRAME_BYTES,
        });
    }
    encoded.push(b'\n');
    writer.write_all(&encoded)?;
    writer.flush()?;
    Ok(())
}

/// Read one frame. `Ok(None)` means the peer closed the connection cleanly.
pub fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, IpcError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    loop {
        let mut buffer = Vec::new();
        let read = {
            let mut limited = reader.by_ref().take(MAX_FRAME_BYTES as u64 + 1);
            limited.read_until(b'\n', &mut buffer)?
        };
        if read == 0 {
            return Ok(None);
        }
        if buffer.len() > MAX_FRAME_BYTES {
            return Err(IpcError::FrameTooLarge {
                limit: MAX_FRAME_BYTES,
            });
        }
        if !buffer.ends_with(b"\n") {
            return Err(IpcError::IncompleteFrame);
        }
        buffer.pop();
        if buffer.ends_with(b"\r") {
            buffer.pop();
        }
        if buffer.iter().all(u8::is_ascii_whitespace) {
            // Tolerate blank lines so a human can poke the socket by hand.
            continue;
        }
        return serde_json::from_slice(&buffer)
            .map(Some)
            .map_err(|error| IpcError::Codec(error.to_string()));
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Probe {
        value: String,
    }

    #[test]
    fn round_trips_frames() {
        let mut buffer = Vec::new();
        write_frame(
            &mut buffer,
            &Probe {
                value: "with\nnewline".to_owned(),
            },
        )
        .unwrap();
        write_frame(
            &mut buffer,
            &Probe {
                value: "second".to_owned(),
            },
        )
        .unwrap();
        // Two frames, two lines, despite the embedded newline.
        assert_eq!(buffer.iter().filter(|b| **b == b'\n').count(), 2);

        let mut reader = BufReader::new(buffer.as_slice());
        let first: Probe = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(first.value, "with\nnewline");
        let second: Probe = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(second.value, "second");
        assert!(read_frame::<_, Probe>(&mut reader).unwrap().is_none());
    }

    #[test]
    fn skips_blank_lines() {
        let raw = b"\n  \n{\"value\":\"ok\"}\n".to_vec();
        let mut reader = BufReader::new(raw.as_slice());
        let frame: Probe = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(frame.value, "ok");
    }

    #[test]
    fn rejects_garbage() {
        let mut reader = BufReader::new(&b"not json\n"[..]);
        assert!(matches!(
            read_frame::<_, Probe>(&mut reader),
            Err(IpcError::Codec(_))
        ));
    }

    #[test]
    fn rejects_a_truncated_frame() {
        let mut reader = BufReader::new(&b"{\"value\":\"no newline\"}"[..]);
        assert!(matches!(
            read_frame::<_, Probe>(&mut reader),
            Err(IpcError::IncompleteFrame)
        ));
    }

    #[test]
    fn rejects_an_oversized_frame() {
        let mut raw = vec![b'x'; MAX_FRAME_BYTES + 10];
        raw.push(b'\n');
        let mut reader = BufReader::new(raw.as_slice());
        assert!(matches!(
            read_frame::<_, Probe>(&mut reader),
            Err(IpcError::FrameTooLarge { .. })
        ));
    }
}
