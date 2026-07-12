//! `SeedLink` selector normalization and incremental packet framing.

const V3_PACKET_BYTES: usize = 520;
const V3_RECORD_OFFSET: usize = 8;
const V4_FIXED_HEADER_BYTES: usize = 17;

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
pub fn drain_records(buffer: &mut Vec<u8>) -> Vec<u8> {
    let mut records = Vec::new();
    loop {
        let Some(position) = find_header(buffer) else {
            let keep_from = buffer.len().saturating_sub(V4_FIXED_HEADER_BYTES - 1);
            buffer.drain(..keep_from);
            break;
        };
        if position > 0 {
            buffer.drain(..position);
        }
        if buffer.len() < 8 {
            break;
        }
        if is_v3_header(buffer) {
            if buffer.len() < V3_PACKET_BYTES {
                break;
            }
            if let Some(record) = buffer.get(V3_RECORD_OFFSET..V3_PACKET_BYTES) {
                records.extend_from_slice(record);
            }
            buffer.drain(..V3_PACKET_BYTES);
        } else if is_v4_header(buffer) {
            if buffer.len() < V4_FIXED_HEADER_BYTES {
                break;
            }
            let Some(length_bytes) = buffer
                .get(4..8)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            else {
                buffer.drain(..1);
                continue;
            };
            let Ok(payload_len) = usize::try_from(u32::from_le_bytes(length_bytes)) else {
                buffer.drain(..1);
                continue;
            };
            let Some(station_len) = buffer.get(16).copied().map(usize::from) else {
                break;
            };
            let packet_len = V4_FIXED_HEADER_BYTES
                .saturating_add(station_len)
                .saturating_add(payload_len);
            if buffer.len() < packet_len {
                break;
            }
            if matches!(buffer.get(2), Some(b'2' | b'3')) && buffer.get(3) == Some(&b'D') {
                let payload_start = V4_FIXED_HEADER_BYTES + station_len;
                if let Some(payload) = buffer.get(payload_start..packet_len) {
                    records.extend_from_slice(payload);
                }
            }
            buffer.drain(..packet_len);
        } else {
            buffer.drain(..1);
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::{drain_records, v4_selectors};

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

        assert!(drain_records(&mut buffer).is_empty());
        assert_eq!(buffer.len(), 100);

        buffer.extend_from_slice(&tail);
        assert_eq!(drain_records(&mut buffer), vec![7; 512]);
        assert!(buffer.is_empty());
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

        assert!(drain_records(&mut buffer).is_empty());
        buffer.extend_from_slice(&tail);
        assert_eq!(drain_records(&mut buffer), payload);
        assert!(buffer.is_empty());
    }
}
