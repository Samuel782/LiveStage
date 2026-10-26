//! ANSI E1.31 (sACN) data packet encoding.

use std::net::Ipv4Addr;

pub const SACN_PORT: u16 = 5568;
const ACN_ID: [u8; 12] = *b"ASC-E1.17\0\0\0";
const VECTOR_ROOT_E131_DATA: u32 = 0x0000_0004;
const VECTOR_E131_DATA_PACKET: u32 = 0x0000_0002;
const VECTOR_DMP_SET_PROPERTY: u8 = 0x02;

/// Standard multicast group for a universe: 239.255.UHI.ULO.
pub fn multicast_addr(universe: u16) -> Ipv4Addr {
    Ipv4Addr::new(239, 255, (universe >> 8) as u8, (universe & 0xFF) as u8)
}

#[derive(Debug, Clone)]
pub struct SacnSource {
    pub cid: [u8; 16],
    pub name: String,
}

impl SacnSource {
    pub fn new(name: &str) -> Self {
        Self { cid: *uuid::Uuid::new_v4().as_bytes(), name: name.to_string() }
    }
}

fn flags_len(len: usize) -> [u8; 2] {
    (0x7000u16 | (len as u16 & 0x0FFF)).to_be_bytes()
}

/// Encodes an E1.31 data packet with start code 0 and `data` slots (≤ 512).
pub fn encode_data_packet(source: &SacnSource, universe: u16, priority: u8, sequence: u8, data: &[u8]) -> Vec<u8> {
    let slots = data.len().min(512);
    let total = 126 + slots;
    let mut p = Vec::with_capacity(total);
    // Root layer
    p.extend_from_slice(&0x0010u16.to_be_bytes());
    p.extend_from_slice(&0x0000u16.to_be_bytes());
    p.extend_from_slice(&ACN_ID);
    p.extend_from_slice(&flags_len(total - 16));
    p.extend_from_slice(&VECTOR_ROOT_E131_DATA.to_be_bytes());
    p.extend_from_slice(&source.cid);
    // Framing layer
    p.extend_from_slice(&flags_len(total - 38));
    p.extend_from_slice(&VECTOR_E131_DATA_PACKET.to_be_bytes());
    let mut name = [0u8; 64];
    let n = source.name.as_bytes();
    let n = &n[..n.len().min(63)];
    name[..n.len()].copy_from_slice(n);
    p.extend_from_slice(&name);
    p.push(priority.min(200));
    p.extend_from_slice(&0u16.to_be_bytes()); // synchronization address
    p.push(sequence);
    p.push(0); // options
    p.extend_from_slice(&universe.to_be_bytes());
    // DMP layer
    p.extend_from_slice(&flags_len(total - 115));
    p.push(VECTOR_DMP_SET_PROPERTY);
    p.push(0xA1);
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&((slots + 1) as u16).to_be_bytes());
    p.push(0x00); // DMX start code
    p.extend_from_slice(&data[..slots]);
    debug_assert_eq!(p.len(), total);
    p
}

/// A received E1.31 data packet (start code 0), borrowing its slots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DataPacket<'a> {
    pub cid: [u8; 16],
    pub priority: u8,
    pub sequence: u8,
    pub options: u8,
    pub universe: u16,
    pub data: &'a [u8],
}

impl DataPacket<'_> {
    /// Preview data: for visualisers, not for live output.
    pub fn preview(&self) -> bool {
        self.options & 0x80 != 0
    }
    /// The source is going away; forget it at once.
    pub fn terminated(&self) -> bool {
        self.options & 0x40 != 0
    }
}

/// Decodes an E1.31 data packet. Other packets (universe discovery,
/// synchronisation, alternate start codes such as per-slot priority) and
/// malformed ones return `None`.
pub fn decode_data_packet(p: &[u8]) -> Option<DataPacket<'_>> {
    let be16 = |i: usize| u16::from_be_bytes([p[i], p[i + 1]]);
    let be32 = |i: usize| u32::from_be_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
    if p.len() < 126 || be16(0) != 0x0010 || be16(2) != 0 || p[4..16] != ACN_ID {
        return None;
    }
    if be32(18) != VECTOR_ROOT_E131_DATA || be32(40) != VECTOR_E131_DATA_PACKET || p[117] != VECTOR_DMP_SET_PROPERTY {
        return None;
    }
    // Address type 0xA1, first address 0, increment 1, start code 0.
    if p[118] != 0xA1 || be16(119) != 0 || be16(121) != 1 || p[125] != 0x00 {
        return None;
    }
    let count = be16(123) as usize; // start code + slots
    if count == 0 || count > 513 || p.len() < 125 + count {
        return None;
    }
    let universe = be16(113);
    if universe == 0 || universe > 63_999 {
        return None;
    }
    Some(DataPacket {
        cid: p[22..38].try_into().ok()?,
        priority: p[108].min(200),
        sequence: p[111],
        options: p[112],
        universe,
        data: &p[126..125 + count],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_what_it_encodes_and_rejects_the_rest() {
        let src = SacnSource { cid: [7; 16], name: "Console".into() };
        let data: Vec<u8> = (0..24).collect();
        let p = encode_data_packet(&src, 42, 150, 9, &data);
        let d = decode_data_packet(&p).unwrap();
        assert_eq!((d.cid, d.priority, d.sequence, d.universe, d.data), ([7; 16], 150, 9, 42, &data[..]));
        assert!(!d.preview() && !d.terminated());
        for len in 0..p.len() {
            assert!(decode_data_packet(&p[..len]).is_none(), "truncated to {len}");
        }
        let mut alt = p.clone();
        alt[125] = 0xDD; // per-slot priority start code
        assert!(decode_data_packet(&alt).is_none());
        let mut bad = p.clone();
        bad[124] = 0xFF; // claims more slots than the packet has
        assert!(decode_data_packet(&bad).is_none());
    }

    #[test]
    fn full_universe_packet_layout() {
        let src = SacnSource { cid: [9; 16], name: "LiveStage".into() };
        let data = [0xAAu8; 512];
        let p = encode_data_packet(&src, 1, 100, 5, &data);
        assert_eq!(p.len(), 638);
        assert_eq!(&p[4..16], b"ASC-E1.17\0\0\0");
        assert_eq!(u16::from_be_bytes([p[16], p[17]]), 0x7000 | 622);
        assert_eq!(u16::from_be_bytes([p[38], p[39]]), 0x7000 | 600);
        assert_eq!(&p[44..53], b"LiveStage");
        assert_eq!(p[108], 100); // priority
        assert_eq!(p[111], 5); // sequence
        assert_eq!(u16::from_be_bytes([p[113], p[114]]), 1); // universe
        assert_eq!(u16::from_be_bytes([p[115], p[116]]), 0x7000 | 523);
        assert_eq!(u16::from_be_bytes([p[123], p[124]]), 513);
        assert_eq!(p[125], 0);
        assert_eq!(p[126], 0xAA);
        assert_eq!(multicast_addr(258), Ipv4Addr::new(239, 255, 1, 2));
    }
}
