//! `SeedLink` selector normalization and incremental packet framing.

/// A `SeedLink` 3 packet: an 8-byte `SLnnnnnn` header and one 512-byte record.
const V3_PACKET_BYTES: usize = 520;
const V3_RECORD_OFFSET: usize = 8;
/// A `SeedLink` 4 header: `SE`, format, subformat, payload length, sequence number
/// and the station-id length; the station id and the payload follow.
const V4_FIXED_HEADER_BYTES: usize = 17;
/// Maximum incomplete packet retained between WebSocket messages.
pub const MAX_FRAME_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Why an advertised `SeedLink` packet cannot be buffered safely.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// A v4 header advertised a packet larger than the framing bound.
    #[error("SeedLink packet advertises {announced} bytes; framing limit is {limit} bytes")]
    Oversized {
        /// Header plus station identifier and payload.
        announced: usize,
        /// Maximum incomplete packet size.
        limit: usize,
    },
    /// Complete records in one WebSocket message exceed the ingest bound.
    #[error("SeedLink message contains {actual} payload bytes; ingest limit is {limit} bytes")]
    PayloadOversized {
        /// Payload bytes decoded from complete packets.
        actual: usize,
        /// Maximum payload returned by one framing pass.
        limit: usize,
    },
}

/// Converts classic channel selectors into `SeedLink` 4 selector syntax.
pub fn v4_selectors(selectors: &str) -> String {
    selectors
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .filter(|part| !part.is_empty())
        .map(v4_selector)
        .collect::<Vec<_>>()
        .join(" ")
}

fn v4_selector(selector: &str) -> String {
    let selector = selector.trim();
    if selector.contains('_') {
        return selector.to_owned();
    }
    let channel = selector
        .split_once('.')
        .map_or(selector, |(channel, _format)| channel);
    let mut characters = channel.chars();
    let band = characters.next().unwrap_or('*');
    let source = characters.next().unwrap_or('*');
    let subsource = characters.next().unwrap_or('*');
    format!("*_{band}_{source}_{subsource}.2D")
}

fn is_v3_header(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SL")
        && bytes
            .get(2..8)
            .is_some_and(|sequence| sequence.iter().all(u8::is_ascii_hexdigit))
}

fn is_v4_header(bytes: &[u8]) -> bool {
    bytes.len() >= V4_FIXED_HEADER_BYTES
        && bytes.first() == Some(&b'S')
        && bytes.get(1) == Some(&b'E')
        && bytes.get(2).is_some_and(u8::is_ascii_alphanumeric)
        && bytes.get(3).is_some_and(u8::is_ascii_alphanumeric)
}

fn find_header(bytes: &[u8]) -> Option<usize> {
    let classic = bytes.windows(8).position(is_v3_header);
    let current = bytes.windows(V4_FIXED_HEADER_BYTES).position(is_v4_header);
    match (classic, current) {
        (Some(classic), Some(current)) => Some(classic.min(current)),
        (Some(classic), None) => Some(classic),
        (None, Some(current)) => Some(current),
        (None, None) => None,
    }
}

/// Removes complete `SeedLink` packets from `buffer` and returns their miniSEED payloads.
///
/// An incomplete trailing packet stays buffered so the next WebSocket message can finish it.
///
/// # Errors
///
/// Returns [`FrameError`] when one packet or the aggregate decoded payload exceeds the bound.
pub fn drain_records(buffer: &mut Vec<u8>) -> Result<Vec<u8>, FrameError> {
    // A consumed-bytes cursor with one drain at the end: a WebSocket message
    // carries back-to-back packets, and per-packet drains plus full-buffer
    // header scans would make each message quadratic in its packet count.
    // find_header runs only to resync after garbage.
    let mut records = Vec::new();
    let mut cursor = 0_usize;
    loop {
        // Find the next header, skipping garbage
        let bytes = buffer.get(cursor..).unwrap_or_default();
        let bytes = if is_v3_header(bytes) || is_v4_header(bytes) {
            bytes
        } else if let Some(position) = find_header(bytes) {
            cursor = cursor.saturating_add(position);
            buffer.get(cursor..).unwrap_or_default()
        } else {
            // No header anywhere: everything but a possible partial-header tail
            // is garbage the next message cannot complete.
            cursor = cursor.saturating_add(bytes.len().saturating_sub(V4_FIXED_HEADER_BYTES - 1));
            break;
        };
        if is_v3_header(bytes) {
            // SeedLink 3 packet
            if bytes.len() < V3_PACKET_BYTES {
                break;
            }
            if let Some(record) = bytes.get(V3_RECORD_OFFSET..V3_PACKET_BYTES) {
                let actual = records.len().saturating_add(record.len());
                if actual > MAX_FRAME_BUFFER_BYTES {
                    buffer.drain(..cursor.min(buffer.len()));
                    return Err(FrameError::PayloadOversized {
                        actual,
                        limit: MAX_FRAME_BUFFER_BYTES,
                    });
                }
                records.extend_from_slice(record);
            }
            cursor = cursor.saturating_add(V3_PACKET_BYTES);
        } else {
            // SeedLink 4 packet
            let Some(length_bytes) = bytes
                .get(4..8)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            else {
                cursor = cursor.saturating_add(1);
                continue;
            };
            let Ok(payload_len) = usize::try_from(u32::from_le_bytes(length_bytes)) else {
                cursor = cursor.saturating_add(1);
                continue;
            };
            let Some(station_len) = bytes.get(16).copied().map(usize::from) else {
                break;
            };
            let packet_len = V4_FIXED_HEADER_BYTES
                .saturating_add(station_len)
                .saturating_add(payload_len);
            if packet_len > MAX_FRAME_BUFFER_BYTES {
                buffer.drain(..cursor.min(buffer.len()));
                return Err(FrameError::Oversized {
                    announced: packet_len,
                    limit: MAX_FRAME_BUFFER_BYTES,
                });
            }
            if bytes.len() < packet_len {
                break;
            }
            // Only miniSEED 2 and 3 data packets (`2D`, `3D`) carry records.
            if matches!(bytes.get(2), Some(b'2' | b'3')) && bytes.get(3) == Some(&b'D') {
                let payload_start = V4_FIXED_HEADER_BYTES + station_len;
                if let Some(payload) = bytes.get(payload_start..packet_len) {
                    let actual = records.len().saturating_add(payload.len());
                    if actual > MAX_FRAME_BUFFER_BYTES {
                        buffer.drain(..cursor.min(buffer.len()));
                        return Err(FrameError::PayloadOversized {
                            actual,
                            limit: MAX_FRAME_BUFFER_BYTES,
                        });
                    }
                    records.extend_from_slice(payload);
                }
            }
            cursor = cursor.saturating_add(packet_len);
        }
    }
    buffer.drain(..cursor.min(buffer.len()));
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::{FrameError, MAX_FRAME_BUFFER_BYTES, drain_records, v4_selectors};

    #[test]
    fn normalizes_classic_selectors() {
        assert_eq!(v4_selectors("BH?, HHZ.D"), "*_B_H_?.2D *_H_H_Z.2D");
        assert_eq!(v4_selectors("XX_STA_00_B_H_Z.2D"), "XX_STA_00_B_H_Z.2D");
    }

    #[test]
    fn retains_a_v3_packet_split_across_messages() {
        let mut packet = b"SL000001".to_vec();
        packet.extend(std::iter::repeat_n(7, 512));
        let tail = packet.split_off(100);
        let mut buffer = packet;

        assert!(drain_records(&mut buffer).is_ok_and(|records| records.is_empty()));
        assert_eq!(buffer.len(), 100);

        buffer.extend_from_slice(&tail);
        assert_eq!(drain_records(&mut buffer), Ok(vec![7; 512]));
        assert_eq!(buffer, Vec::<u8>::new());
    }

    #[test]
    fn retains_a_v4_packet_split_across_messages() {
        let payload = [1, 2, 3, 4];
        let station = b"STA";
        let mut packet = b"SE2D".to_vec();
        packet.extend_from_slice(&4_u32.to_le_bytes());
        packet.extend_from_slice(&[0; 8]);
        packet.push(3_u8);
        packet.extend_from_slice(station);
        packet.extend_from_slice(&payload);
        let split = packet.len() - 2;
        let tail = packet.split_off(split);
        let mut buffer = packet;

        assert!(drain_records(&mut buffer).is_ok_and(|records| records.is_empty()));
        buffer.extend_from_slice(&tail);
        assert_eq!(drain_records(&mut buffer), Ok(payload.to_vec()));
        assert_eq!(buffer, Vec::<u8>::new());
    }

    #[test]
    fn rejects_an_incomplete_v4_packet_beyond_the_framing_bound() {
        let payload_len = u32::try_from(MAX_FRAME_BUFFER_BYTES).unwrap_or(u32::MAX);
        let mut packet = b"SE2D".to_vec();
        packet.extend_from_slice(&payload_len.to_le_bytes());
        packet.extend_from_slice(&[0; 8]);
        packet.push(0);

        assert!(matches!(
            drain_records(&mut packet),
            Err(FrameError::Oversized { .. })
        ));
    }

    #[test]
    fn rejects_aggregate_complete_records_beyond_the_ingest_bound() {
        let packet_count = MAX_FRAME_BUFFER_BYTES / 512 + 1;
        let mut buffer = Vec::with_capacity(packet_count * 520);
        for sequence in 0..packet_count {
            buffer.extend_from_slice(format!("SL{sequence:06X}").as_bytes());
            buffer.extend(std::iter::repeat_n(7, 512));
        }

        assert!(matches!(
            drain_records(&mut buffer),
            Err(FrameError::PayloadOversized { .. })
        ));
    }
}
