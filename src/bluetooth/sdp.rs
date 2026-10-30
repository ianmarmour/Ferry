//! ## Service Discovery
//!
//! Encoding and parsing of SDP data elements (Bluetooth Core, Vol 3, Part B, section
//! 3), and the ServiceSearchAttribute transaction (section 4.7) used to find the RFCOMM
//! channel of a phone's service, for the platforms where Tidings runs SDP itself.
//!
#![cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]

use std::collections::HashMap;

/// A 16-bit UUID.
pub fn uuid16(uuid: u16) -> Vec<u8> {
    let mut out = vec![0x19];
    out.extend_from_slice(&uuid.to_be_bytes());
    out
}

/// A 16-bit unsigned integer.
pub fn uint16(value: u16) -> Vec<u8> {
    let mut out = vec![0x09];
    out.extend_from_slice(&value.to_be_bytes());
    out
}

/// A 32-bit unsigned integer.
pub fn uint32(value: u32) -> Vec<u8> {
    let mut out = vec![0x0A];
    out.extend_from_slice(&value.to_be_bytes());
    out
}

/// A text string.
pub fn text(value: &str) -> Vec<u8> {
    let mut out = vec![0x25, value.len() as u8];
    out.extend_from_slice(value.as_bytes());
    out
}

/// A data element sequence.
pub fn sequence(elements: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = elements.concat();
    let mut out = vec![0x35, body.len() as u8];
    out.extend(body);
    out
}

/// Reads an unsigned integer element, as found in an attribute value.
pub fn read_uint(element: &[u8]) -> Option<u64> {
    match parse(element)?.0 {
        Element::Uint(value) => u64::try_from(value).ok(),
        _ => None,
    }
}

/// A data element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    /// Nil.
    Nil,
    /// An unsigned integer.
    Uint(u128),
    /// A signed integer.
    Int(i128),
    /// A UUID of 2, 4 or 16 bytes.
    Uuid(Vec<u8>),
    /// A text string.
    Text(Vec<u8>),
    /// A boolean.
    Bool(bool),
    /// A sequence.
    Sequence(Vec<Element>),
    /// An alternative.
    Alternative(Vec<Element>),
    /// A URL.
    Url(Vec<u8>),
}

/// The Bluetooth base UUID, `00000000-0000-1000-8000-00805F9B34FB`, without its first
/// four bytes.
const BASE_UUID: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB,
];

impl Element {
    /// The short form of a UUID element.
    pub fn uuid32(&self) -> Option<u32> {
        let Element::Uuid(bytes) = self else {
            return None;
        };
        match bytes.len() {
            2 => Some(u32::from(u16::from_be_bytes([bytes[0], bytes[1]]))),
            4 => Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
            16 if bytes[4..] == BASE_UUID => {
                Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            _ => None,
        }
    }

    /// The value of an unsigned integer element.
    pub fn uint(&self) -> Option<u128> {
        match self {
            Element::Uint(value) => Some(*value),
            _ => None,
        }
    }

    /// The elements of a sequence or alternative.
    pub fn items(&self) -> &[Element] {
        match self {
            Element::Sequence(items) | Element::Alternative(items) => items,
            _ => &[],
        }
    }
}

/// Parses one data element, returning it and the bytes after it.
pub fn parse(bytes: &[u8]) -> Option<(Element, &[u8])> {
    let (&header, rest) = bytes.split_first()?;
    let kind = header >> 3;
    let (length, rest) = match header & 0x07 {
        0 if kind == 0 => (0, rest),
        0 => (1, rest),
        1 => (2, rest),
        2 => (4, rest),
        3 => (8, rest),
        4 => (16, rest),
        5 => {
            let (&length, rest) = rest.split_first()?;
            (usize::from(length), rest)
        }
        6 => {
            let length = u16::from_be_bytes(rest.get(..2)?.try_into().ok()?);
            (usize::from(length), &rest[2..])
        }
        _ => {
            let length = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
            (usize::try_from(length).ok()?, &rest[4..])
        }
    };
    let data = rest.get(..length)?;
    let rest = &rest[length..];

    let number = |data: &[u8]| {
        data.iter()
            .fold(0u128, |acc, &byte| acc << 8 | u128::from(byte))
    };
    let element = match kind {
        0 => Element::Nil,
        1 if length <= 16 => Element::Uint(number(data)),
        2 if (1..=16).contains(&length) => {
            let shift = 128 - 8 * length as u32;
            Element::Int(((number(data) << shift) as i128) >> shift)
        }
        3 if matches!(length, 2 | 4 | 16) => Element::Uuid(data.to_vec()),
        4 => Element::Text(data.to_vec()),
        5 => Element::Bool(data.first().is_some_and(|&value| value != 0)),
        6 => Element::Sequence(parse_all(data)?),
        7 => Element::Alternative(parse_all(data)?),
        8 => Element::Url(data.to_vec()),
        _ => return None,
    };
    Some((element, rest))
}

fn parse_all(mut data: &[u8]) -> Option<Vec<Element>> {
    let mut elements = Vec::new();
    while !data.is_empty() {
        let (element, rest) = parse(data)?;
        elements.push(element);
        data = rest;
    }
    Some(elements)
}

/// A service record: its attributes by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record(pub HashMap<u16, Element>);

/// `ProtocolDescriptorList`.
pub const PROTOCOL_DESCRIPTOR_LIST: u16 = 0x0004;

/// The RFCOMM protocol UUID.
pub const RFCOMM: u32 = 0x0003;

impl Record {
    /// The RFCOMM channel of the service, if it is reachable over RFCOMM.
    pub fn rfcomm_channel(&self) -> Option<u8> {
        let list = self.0.get(&PROTOCOL_DESCRIPTOR_LIST)?;
        // The list is a sequence of protocols, or an alternative of such sequences.
        let stacks: Vec<&Element> = match list {
            Element::Alternative(stacks) => stacks.iter().collect(),
            stack => vec![stack],
        };
        stacks
            .into_iter()
            .flat_map(Element::items)
            .find_map(|protocol| {
                let items = protocol.items();
                (items.first()?.uuid32()? == RFCOMM)
                    .then(|| items.get(1)?.uint())
                    .flatten()
                    .and_then(|channel| u8::try_from(channel).ok())
            })
    }

    /// An unsigned integer attribute.
    pub fn uint(&self, attribute: u16) -> Option<u128> {
        self.0.get(&attribute)?.uint()
    }
}

/// Parses the `AttributeLists` of a ServiceSearchAttribute response.
pub fn records(attribute_lists: &[u8]) -> Option<Vec<Record>> {
    let (lists, _) = parse(attribute_lists)?;
    lists
        .items()
        .iter()
        .map(|list| {
            let items = list.items();
            let mut record = HashMap::new();
            for pair in items.chunks(2) {
                if let [id, value] = pair {
                    record.insert(u16::try_from(id.uint()?).ok()?, value.clone());
                }
            }
            Some(Record(record))
        })
        .collect()
}

/// A ServiceSearchAttribute request for every attribute of the services of class
/// `uuid`, continuing after `continuation`.
pub fn search_request(transaction: u16, uuid: u16, continuation: &[u8]) -> Vec<u8> {
    let mut parameters = sequence(&[uuid16(uuid)]);
    parameters.extend_from_slice(&u16::MAX.to_be_bytes());
    parameters.extend(sequence(&[uint32(0x0000_FFFF)]));
    parameters.push(continuation.len() as u8);
    parameters.extend_from_slice(continuation);

    let mut pdu = vec![0x06];
    pdu.extend_from_slice(&transaction.to_be_bytes());
    pdu.extend_from_slice(&(parameters.len() as u16).to_be_bytes());
    pdu.extend(parameters);
    pdu
}

/// Splits a ServiceSearchAttribute response into its part of the attribute lists and
/// its continuation state, empty on the last response.
pub fn search_response(pdu: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let malformed = || "malformed SDP response".to_string();
    match pdu.first() {
        Some(0x07) => {}
        Some(0x01) => {
            let code = pdu
                .get(5..7)
                .map(|code| u16::from_be_bytes([code[0], code[1]]));
            return Err(format!(
                "the phone's SDP server refused the search ({:?})",
                code
            ));
        }
        _ => return Err(malformed()),
    }
    let count = usize::from(u16::from_be_bytes(
        pdu.get(5..7).ok_or_else(malformed)?.try_into().unwrap(),
    ));
    let lists = pdu.get(7..7 + count).ok_or_else(malformed)?;
    let continuation_length = usize::from(*pdu.get(7 + count).ok_or_else(malformed)?);
    let continuation = pdu
        .get(8 + count..8 + count + continuation_length)
        .ok_or_else(malformed)?;
    Ok((lists, continuation))
}

/// A response to [`search_request`], for tests and the simulated phone.
pub fn search_response_pdu(transaction: u16, lists: &[u8], continuation: &[u8]) -> Vec<u8> {
    let mut parameters = (lists.len() as u16).to_be_bytes().to_vec();
    parameters.extend_from_slice(lists);
    parameters.push(continuation.len() as u8);
    parameters.extend_from_slice(continuation);

    let mut pdu = vec![0x07];
    pdu.extend_from_slice(&transaction.to_be_bytes());
    pdu.extend_from_slice(&(parameters.len() as u16).to_be_bytes());
    pdu.extend(parameters);
    pdu
}

/// The attribute lists of services reachable over RFCOMM `channel`, with `class` as
/// their service class and MAP's SupportedMessageTypes set to `message_types`.
pub fn rfcomm_service(class: u16, channel: u8, message_types: Option<u8>) -> Vec<u8> {
    let mut attributes = vec![
        uint16(0x0001),
        sequence(&[uuid16(class)]),
        uint16(PROTOCOL_DESCRIPTOR_LIST),
        sequence(&[
            sequence(&[uuid16(0x0100)]),
            sequence(&[uuid16(RFCOMM as u16), vec![0x08, channel]]),
            sequence(&[uuid16(0x0008)]),
        ]),
    ];
    if let Some(types) = message_types {
        attributes.push(uint16(map::sdp::ATTRIBUTE_SUPPORTED_MESSAGE_TYPES));
        attributes.push(vec![0x08, types]);
    }
    sequence(&attributes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_reads_elements() {
        assert_eq!(
            vec![0x35, 0x06, 0x19, 0x11, 0x34, 0x09, 0x01, 0x04],
            sequence(&[uuid16(0x1134), uint16(0x0104)])
        );
        assert_eq!(Some(0x1F), read_uint(&uint32(0x1F)));
        assert_eq!(Some(3), read_uint(&[0x08, 3]));
        assert_eq!(None, read_uint(&text("x")));
    }

    #[test]
    fn parses_every_size_and_long_uuids() {
        let (element, rest) = parse(&[0x0A, 0, 0, 1, 2, 0xFF]).unwrap();
        assert_eq!(Element::Uint(0x0102), element);
        assert_eq!(&[0xFF], rest);

        assert_eq!(Element::Int(-2), parse(&[0x10, 0xFE]).unwrap().0);

        let mut long = vec![0x1C, 0x00, 0x00, 0x11, 0x32];
        long.extend_from_slice(&BASE_UUID);
        assert_eq!(Some(0x1132), parse(&long).unwrap().0.uuid32());

        // A sequence with a 16-bit length.
        let (element, _) = parse(&[0x36, 0x00, 0x03, 0x19, 0x01, 0x00]).unwrap();
        assert_eq!(Some(0x0100), element.items()[0].uuid32());
        assert_eq!(None, parse(&[0x35, 0x05, 0x19]));
    }

    #[test]
    fn finds_rfcomm_channels_through_a_search() {
        let request = search_request(7, 0x1132, &[]);
        assert_eq!(
            vec![
                0x06, 0, 7, 0, 15, 0x35, 3, 0x19, 0x11, 0x32, 0xFF, 0xFF, 0x35, 5, 0x0A, 0, 0,
                0xFF, 0xFF, 0
            ],
            request
        );

        let lists = sequence(&[
            rfcomm_service(0x1132, 2, Some(0x0E)),
            rfcomm_service(0x1132, 5, Some(0x01)),
        ]);
        let (first, second) = lists.split_at(9);
        let pdu = search_response_pdu(7, first, &[0xAA]);
        assert_eq!(Ok((first, &[0xAA][..])), search_response(&pdu));
        let pdu = search_response_pdu(8, second, &[]);
        assert_eq!(Ok((second, &[][..])), search_response(&pdu));

        let records = records(&lists).unwrap();
        assert_eq!(2, records.len());
        assert_eq!(Some(2), records[0].rfcomm_channel());
        assert_eq!(
            Some(0x0E),
            records[0].uint(map::sdp::ATTRIBUTE_SUPPORTED_MESSAGE_TYPES)
        );
        assert_eq!(Some(5), records[1].rfcomm_channel());

        assert!(search_response(&[0x01, 0, 7, 0, 2, 0, 3]).is_err());
    }
}
