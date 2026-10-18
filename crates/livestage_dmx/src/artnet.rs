//! Art-Net 4 packet encoding.

pub const ARTNET_PORT: u16 = 6454;
const ID: &[u8; 8] = b"Art-Net\0";
const PROTOCOL_VERSION: u16 = 14;
pub const OP_POLL: u16 = 0x2000;
pub const OP_DMX: u16 = 0x5000;

/// Encodes an ArtDmx packet. `port_address` is the 15-bit
/// Net(7) : SubNet(4) : Universe(4) address. Data is padded to an even
/// length between 2 and 512 as required by the spec.
pub fn encode_art_dmx(sequence: u8, physical: u8, port_address: u16, data: &[u8]) -> Vec<u8> {
    let mut len = data.len().clamp(2, 512);
    len += len % 2;
    let mut p = Vec::with_capacity(18 + len);
    p.extend_from_slice(ID);
    p.extend_from_slice(&OP_DMX.to_le_bytes());
    p.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    p.push(sequence);
    p.push(physical);
    p.push((port_address & 0xFF) as u8); // SubUni
    p.push(((port_address >> 8) & 0x7F) as u8); // Net
    p.extend_from_slice(&(len as u16).to_be_bytes());
    p.extend_from_slice(&data[..data.len().min(len)]);
    p.resize(18 + len, 0);
    p
}

/// ArtPoll for node discovery (TalkToMe = send ArtPollReply on change).
pub fn encode_art_poll() -> Vec<u8> {
    let mut p = Vec::with_capacity(14);
    p.extend_from_slice(ID);
    p.extend_from_slice(&OP_POLL.to_le_bytes());
    p.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    p.push(0b0000_0010);
    p.push(0x10); // DiagPriority: low
    p
}

/// Builds a port address from its components.
pub fn port_address(net: u8, subnet: u8, universe: u8) -> u16 {
    ((net as u16 & 0x7F) << 8) | ((subnet as u16 & 0x0F) << 4) | (universe as u16 & 0x0F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_dmx_layout() {
        let data = [1u8, 2, 3];
        let p = encode_art_dmx(7, 0, port_address(1, 2, 3), &data);
        assert_eq!(&p[0..8], b"Art-Net\0");
        assert_eq!(&p[8..10], &[0x00, 0x50]); // OpCode LE
        assert_eq!(&p[10..12], &[0, 14]);
        assert_eq!(p[12], 7);
        assert_eq!(p[14], 0x23); // SubUni
        assert_eq!(p[15], 0x01); // Net
        assert_eq!(&p[16..18], &[0, 4]); // padded to even
        assert_eq!(&p[18..], &[1, 2, 3, 0]);
    }
}
