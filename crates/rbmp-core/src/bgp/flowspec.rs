use serde::{Deserialize, Serialize};
use crate::{Error, Result};

/// A single Flowspec component (RFC 5575 for IPv4, RFC 8955 for IPv6)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FlowspecComponent {
    /// Type 1: Destination prefix
    DestPrefix  { prefix: String, prefix_len: u8 },
    /// Type 2: Source prefix
    SrcPrefix   { prefix: String, prefix_len: u8 },
    /// Type 3: IP Protocol
    IpProtocol  { ops: Vec<NumericOp> },
    /// Type 4: Port
    Port        { ops: Vec<NumericOp> },
    /// Type 5: Destination port
    DstPort     { ops: Vec<NumericOp> },
    /// Type 6: Source port
    SrcPort     { ops: Vec<NumericOp> },
    /// Type 7: ICMP type
    IcmpType    { ops: Vec<NumericOp> },
    /// Type 8: ICMP code
    IcmpCode    { ops: Vec<NumericOp> },
    /// Type 9: TCP flags (bitmask operator)
    TcpFlags    { ops: Vec<BitmaskOp> },
    /// Type 10: Packet length
    PktLen      { ops: Vec<NumericOp> },
    /// Type 11: DSCP
    Dscp        { ops: Vec<NumericOp> },
    /// Type 12: Fragment flags
    Fragment    { ops: Vec<BitmaskOp> },
    Unknown     { component_type: u8, data: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumericOp {
    pub lt:      bool,
    pub gt:      bool,
    pub eq:      bool,
    pub and_bit: bool,
    pub value:   u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitmaskOp {
    pub not:       bool,
    pub match_bit: bool,
    pub and_bit:   bool,
    pub value:     u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowspecNlri {
    pub components: Vec<FlowspecComponent>,
    /// Human-readable summary: e.g. "dst=192.0.2.0/24 proto=6 dstport=80"
    pub summary:    String,
}

/// Decode Flowspec NLRIs from a byte buffer.
/// Each NLRI is length-prefixed (1 or 2 bytes if first byte >= 0xF0).
pub fn decode_flowspec_nlri(mut buf: &[u8], afi_is_ipv6: bool) -> Result<Vec<FlowspecNlri>> {
    let mut result = Vec::new();
    while !buf.is_empty() {
        let (length, header_size) = if buf[0] < 0xF0 {
            (buf[0] as usize, 1)
        } else if buf.len() >= 2 {
            (((buf[0] as usize & 0x0F) << 8) | buf[1] as usize, 2)
        } else {
            break;
        };
        if buf.len() < header_size + length { break; }
        let nlri_bytes = &buf[header_size..header_size + length];
        buf = &buf[header_size + length..];
        result.push(parse_flowspec_nlri(nlri_bytes, afi_is_ipv6)?);
    }
    Ok(result)
}

fn parse_flowspec_nlri(mut buf: &[u8], afi_is_ipv6: bool) -> Result<FlowspecNlri> {
    let mut components = Vec::new();
    let mut summary_parts = Vec::new();
    while !buf.is_empty() {
        let comp_type = buf[0];
        buf = &buf[1..];
        let (comp, summary, consumed) = parse_flowspec_component(comp_type, buf, afi_is_ipv6)?;
        buf = &buf[consumed..];
        summary_parts.push(summary);
        components.push(comp);
    }
    Ok(FlowspecNlri { components, summary: summary_parts.join(" ") })
}

fn parse_flowspec_component(t: u8, buf: &[u8], ipv6: bool) -> Result<(FlowspecComponent, String, usize)> {
    match t {
        1 | 2 => {
            // Destination (1) or Source (2) prefix: prefix_len(1) + prefix_bytes
            if buf.is_empty() {
                return Err(Error::UnexpectedEof { needed: 1, have: 0 });
            }
            let prefix_len = buf[0];
            let octets = (prefix_len as usize + 7) / 8;
            if buf.len() < 1 + octets {
                return Err(Error::UnexpectedEof { needed: 1 + octets, have: buf.len() });
            }
            let prefix = if ipv6 {
                let mut a = [0u8; 16];
                a[..octets].copy_from_slice(&buf[1..1+octets]);
                std::net::Ipv6Addr::from(a).to_string()
            } else {
                let mut a = [0u8; 4];
                let n = octets.min(4);
                a[..n].copy_from_slice(&buf[1..1+n]);
                std::net::Ipv4Addr::from(a).to_string()
            };
            let label = if t == 1 { "dst" } else { "src" };
            let summary = format!("{label}={prefix}/{prefix_len}");
            let comp = if t == 1 {
                FlowspecComponent::DestPrefix { prefix, prefix_len }
            } else {
                FlowspecComponent::SrcPrefix { prefix, prefix_len }
            };
            Ok((comp, summary, 1 + octets))
        }
        3..=8 | 10 | 11 => {
            let (ops, consumed) = parse_numeric_ops(buf)?;
            let label = match t {
                3 => "proto", 4 => "port", 5 => "dstport", 6 => "srcport",
                7 => "icmptype", 8 => "icmpcode", 10 => "pktlen", _ => "dscp",
            };
            let summary = format!("{}={}", label,
                ops.iter().map(|o| o.value.to_string()).collect::<Vec<_>>().join(","));
            let comp = match t {
                3  => FlowspecComponent::IpProtocol { ops },
                4  => FlowspecComponent::Port { ops },
                5  => FlowspecComponent::DstPort { ops },
                6  => FlowspecComponent::SrcPort { ops },
                7  => FlowspecComponent::IcmpType { ops },
                8  => FlowspecComponent::IcmpCode { ops },
                10 => FlowspecComponent::PktLen { ops },
                _  => FlowspecComponent::Dscp { ops },
            };
            Ok((comp, summary, consumed))
        }
        9 | 12 => {
            let (ops, consumed) = parse_bitmask_ops(buf)?;
            let label = if t == 9 { "tcpflags" } else { "fragment" };
            let summary = format!("{}=0x{:x}", label,
                ops.first().map(|o| o.value).unwrap_or(0));
            let comp = if t == 9 {
                FlowspecComponent::TcpFlags { ops }
            } else {
                FlowspecComponent::Fragment { ops }
            };
            Ok((comp, summary, consumed))
        }
        _ => {
            Ok((FlowspecComponent::Unknown { component_type: t, data: buf.to_vec() },
                format!("unknown-type-{t}"), buf.len()))
        }
    }
}

fn parse_numeric_ops(buf: &[u8]) -> Result<(Vec<NumericOp>, usize)> {
    let mut ops = Vec::new();
    let mut pos = 0;
    loop {
        if pos >= buf.len() { break; }
        let op_byte  = buf[pos]; pos += 1;
        let eol      = op_byte & 0x80 != 0;
        let and_bit  = op_byte & 0x40 != 0;
        let len_code = (op_byte >> 4) & 0x03;
        let lt       = op_byte & 0x04 != 0;
        let gt       = op_byte & 0x02 != 0;
        let eq       = op_byte & 0x01 != 0;
        let vlen     = 1usize << len_code;
        if pos + vlen > buf.len() { break; }
        let value = match vlen {
            1 => buf[pos] as u64,
            2 => u16::from_be_bytes([buf[pos], buf[pos+1]]) as u64,
            4 => u32::from_be_bytes(buf[pos..pos+4].try_into().unwrap()) as u64,
            8 => u64::from_be_bytes(buf[pos..pos+8].try_into().unwrap()),
            _ => 0,
        };
        pos += vlen;
        ops.push(NumericOp { lt, gt, eq, and_bit, value });
        if eol { break; }
    }
    Ok((ops, pos))
}

fn parse_bitmask_ops(buf: &[u8]) -> Result<(Vec<BitmaskOp>, usize)> {
    let mut ops = Vec::new();
    let mut pos = 0;
    loop {
        if pos >= buf.len() { break; }
        let op_byte   = buf[pos]; pos += 1;
        let eol       = op_byte & 0x80 != 0;
        let and_bit   = op_byte & 0x40 != 0;
        let len_code  = (op_byte >> 4) & 0x03;
        let not_bit   = op_byte & 0x02 != 0;
        let match_bit = op_byte & 0x01 != 0;
        let vlen      = 1usize << len_code;
        if pos + vlen > buf.len() { break; }
        let value = match vlen {
            1 => buf[pos] as u64,
            2 => u16::from_be_bytes([buf[pos], buf[pos+1]]) as u64,
            _ => 0,
        };
        pos += vlen;
        ops.push(BitmaskOp { not: not_bit, match_bit, and_bit, value });
        if eol { break; }
    }
    Ok((ops, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flowspec_dest_prefix_ipv4() {
        // NLRI body: type=1 (dst), prefix_len=24, 3 prefix bytes = 5 body bytes
        // Full buffer: 1 length byte + 5 body bytes
        let buf = [5u8, 1, 24, 192, 0, 2];
        let nlris = decode_flowspec_nlri(&buf, false).unwrap();
        assert_eq!(nlris.len(), 1);
        assert_eq!(nlris[0].summary, "dst=192.0.2.0/24");
    }

    #[test]
    fn test_flowspec_protocol_tcp() {
        // NLRI body: type=3 (proto), op_byte=0x81, val=6 = 3 body bytes
        // op_byte: 0x81 = 1000_0001 = eol(1) and(0) len(00=1byte) lt(0) gt(0) eq(1)
        // Full buffer: 1 length byte + 3 body bytes
        let buf = [3u8, 3, 0x81, 6];
        let nlris = decode_flowspec_nlri(&buf, false).unwrap();
        assert_eq!(nlris.len(), 1);
        assert!(nlris[0].summary.starts_with("proto=6"));
    }

    #[test]
    fn test_flowspec_tcp_flags() {
        // NLRI body: type=9 (tcpflags), op_byte=0x81, value=0x02 = 3 body bytes
        // op_byte: 0x81 = 1000_0001 = eol(1) and(0) len(00=1byte) reserved(0) not(0) match(1)
        // value=0x02 (SYN flag). Full buffer: 1 length byte + 3 body bytes
        let buf = [3u8, 9, 0x81, 0x02];
        let nlris = decode_flowspec_nlri(&buf, false).unwrap();
        assert_eq!(nlris.len(), 1);
        assert!(nlris[0].summary.starts_with("tcpflags="));
    }
}
