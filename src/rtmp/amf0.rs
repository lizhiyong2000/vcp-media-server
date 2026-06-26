/// AMF0 (Action Message Format) encoder and decoder
/// Supports Number, Boolean, String, Object, Null, Undefined, ECMA Array types.
use std::collections::HashMap;

/// AMF0 value types
#[derive(Debug, Clone, PartialEq)]
pub enum Amf0Value {
    Number(f64),
    Boolean(bool),
    String(String),
    Object(HashMap<String, Amf0Value>),
    Null,
    Undefined,
    EcmaArray(HashMap<String, Amf0Value>),
}

impl Amf0Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Amf0Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Amf0Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Amf0Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&HashMap<String, Amf0Value>> {
        match self {
            Amf0Value::Object(m) | Amf0Value::EcmaArray(m) => Some(m),
            _ => None,
        }
    }
}

/// AMF0 type markers
const AMF0_NUMBER: u8 = 0x00;
const AMF0_BOOLEAN: u8 = 0x01;
const AMF0_STRING: u8 = 0x02;
const AMF0_OBJECT: u8 = 0x03;
const AMF0_NULL: u8 = 0x05;
const AMF0_UNDEFINED: u8 = 0x06;
const AMF0_ECMA_ARRAY: u8 = 0x08;
const AMF0_OBJECT_END: u8 = 0x09;

/// Decode AMF0 values from a byte slice, returns (values, bytes_consumed)
pub fn decode(data: &[u8]) -> Result<(Vec<Amf0Value>, usize), String> {
    let mut values = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        // Check for object-end marker (0x00 0x00 0x09)
        if offset + 3 <= data.len() && data[offset] == 0x00 && data[offset + 1] == 0x00 && data[offset + 2] == 0x09 {
            offset += 3;
            break;
        }

        match decode_value(data, &mut offset) {
            Ok(val) => values.push(val),
            Err(e) => {
                if values.is_empty() {
                    return Err(e);
                }
                break;
            }
        }
    }

    Ok((values, offset))
}

/// Decode a single AMF0 value
fn decode_value(data: &[u8], offset: &mut usize) -> Result<Amf0Value, String> {
    if *offset >= data.len() {
        return Err("Not enough data for AMF0 marker".to_string());
    }

    let marker = data[*offset];
    *offset += 1;

    match marker {
        AMF0_NUMBER => {
            if *offset + 8 > data.len() {
                return Err("Not enough data for Number".to_string());
            }
            let bytes: [u8; 8] = data[*offset..*offset + 8].try_into().unwrap();
            let val = f64::from_be_bytes(bytes);
            *offset += 8;
            Ok(Amf0Value::Number(val))
        }
        AMF0_BOOLEAN => {
            if *offset + 1 > data.len() {
                return Err("Not enough data for Boolean".to_string());
            }
            let val = data[*offset] != 0;
            *offset += 1;
            Ok(Amf0Value::Boolean(val))
        }
        AMF0_STRING => {
            decode_string(data, offset).map(Amf0Value::String)
        }
        AMF0_OBJECT => {
            decode_object(data, offset)
        }
        AMF0_NULL | AMF0_UNDEFINED => {
            Ok(Amf0Value::Null)
        }
        AMF0_ECMA_ARRAY => {
            if *offset + 4 > data.len() {
                return Err("Not enough data for ECMA array count".to_string());
            }
            // Read approximate count (we ignore it and read until object-end)
            *offset += 4;
            decode_object_inner(data, offset, true)
        }
        _ => {
            Err(format!("Unsupported AMF0 type: 0x{:02X} at offset {}", marker, *offset - 1))
        }
    }
}

/// Decode an AMF0 string (2-byte length + UTF-8 data)
fn decode_string(data: &[u8], offset: &mut usize) -> Result<String, String> {
    if *offset + 2 > data.len() {
        return Err("Not enough data for string length".to_string());
    }
    let len = ((data[*offset] as usize) << 8) | data[*offset + 1] as usize;
    *offset += 2;

    if *offset + len > data.len() {
        return Err(format!("Not enough data for string: need {} but have {}", len, data.len() - *offset));
    }

    let s = String::from_utf8_lossy(&data[*offset..*offset + len]).to_string();
    *offset += len;
    Ok(s)
}

/// Decode an AMF0 object
fn decode_object(data: &[u8], offset: &mut usize) -> Result<Amf0Value, String> {
    decode_object_inner(data, offset, false)
}

/// Internal object decoder (shared between Object and ECMA Array)
fn decode_object_inner(data: &[u8], offset: &mut usize, is_ecma: bool) -> Result<Amf0Value, String> {
    let mut map = HashMap::new();

    loop {
        // Check for object-end marker (3 bytes: 0x00 0x00 0x09)
        if *offset + 3 <= data.len()
            && data[*offset] == 0x00
            && data[*offset + 1] == 0x00
            && data[*offset + 2] == 0x09
        {
            *offset += 3;
            break;
        }

        // Read property name (string without marker)
        if *offset + 2 > data.len() {
            return Err("Not enough data for property name".to_string());
        }
        let key_len = ((data[*offset] as usize) << 8) | data[*offset + 1] as usize;
        *offset += 2;

        if key_len == 0 {
            // Empty key + end marker
            if *offset < data.len() && data[*offset] == AMF0_OBJECT_END {
                *offset += 1;
            }
            break;
        }

        if *offset + key_len > data.len() {
            return Err("Not enough data for property name".to_string());
        }
        let key = String::from_utf8_lossy(&data[*offset..*offset + key_len]).to_string();
        *offset += key_len;

        // Read property value
        match decode_value(data, offset) {
            Ok(val) => { map.insert(key, val); }
            Err(_) => break,
        }
    }

    if is_ecma {
        Ok(Amf0Value::EcmaArray(map))
    } else {
        Ok(Amf0Value::Object(map))
    }
}

/// Encode AMF0 values to bytes
pub fn encode(values: &[Amf0Value]) -> Vec<u8> {
    let mut buf = Vec::new();
    for val in values {
        encode_value(&mut buf, val);
    }
    buf
}

/// Encode a single AMF0 value
pub fn encode_value(buf: &mut Vec<u8>, val: &Amf0Value) {
    match val {
        Amf0Value::Number(n) => {
            buf.push(AMF0_NUMBER);
            buf.extend_from_slice(&n.to_be_bytes());
        }
        Amf0Value::Boolean(b) => {
            buf.push(AMF0_BOOLEAN);
            buf.push(if *b { 1 } else { 0 });
        }
        Amf0Value::String(s) => {
            buf.push(AMF0_STRING);
            let len = s.len() as u16;
            buf.push((len >> 8) as u8);
            buf.push((len & 0xFF) as u8);
            buf.extend_from_slice(s.as_bytes());
        }
        Amf0Value::Object(map) => {
            buf.push(AMF0_OBJECT);
            encode_object_properties(buf, map);
            // Object end marker
            buf.push(0x00);
            buf.push(0x00);
            buf.push(AMF0_OBJECT_END);
        }
        Amf0Value::Null => {
            buf.push(AMF0_NULL);
        }
        Amf0Value::Undefined => {
            buf.push(AMF0_UNDEFINED);
        }
        Amf0Value::EcmaArray(map) => {
            buf.push(AMF0_ECMA_ARRAY);
            let count = map.len() as u32;
            buf.extend_from_slice(&count.to_be_bytes());
            encode_object_properties(buf, map);
            // Object end marker
            buf.push(0x00);
            buf.push(0x00);
            buf.push(AMF0_OBJECT_END);
        }
    }
}

/// Encode object properties
fn encode_object_properties(buf: &mut Vec<u8>, map: &HashMap<String, Amf0Value>) {
    for (key, val) in map {
        // Key (string without marker, 2-byte length prefix)
        let key_len = key.len() as u16;
        buf.push((key_len >> 8) as u8);
        buf.push((key_len & 0xFF) as u8);
        buf.extend_from_slice(key.as_bytes());
        // Value
        encode_value(buf, val);
    }
}

/// Helper: encode a command name string (for RTMP commands)
pub fn encode_command(name: &str) -> Vec<u8> {
    encode(&[Amf0Value::String(name.to_string())])
}

/// Helper: parse an AMF0 command and extract name + arguments
pub fn parse_command(payload: &[u8]) -> Result<(String, Vec<Amf0Value>), String> {
    let (values, _) = decode(payload)?;
    if values.is_empty() {
        return Err("Empty AMF0 payload".to_string());
    }

    let command = match &values[0] {
        Amf0Value::String(s) => s.clone(),
        _ => return Err("First AMF0 value must be a string command name".to_string()),
    };

    Ok((command, values[1..].to_vec()))
}

/// Extract a string property from an Amf0Value (usually an Object)
pub fn get_string_prop(val: &Amf0Value, key: &str) -> Option<String> {
    if let Some(map) = val.as_object() {
        map.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    } else {
        None
    }
}

/// Extract a number property from an Amf0Value
pub fn get_number_prop(val: &Amf0Value, key: &str) -> Option<f64> {
    if let Some(map) = val.as_object() {
        map.get(key).and_then(|v| v.as_f64())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_number() {
        let val = Amf0Value::Number(42.0);
        let encoded = encode(&[val.clone()]);
        let (decoded, _) = decode(&encoded).unwrap();
        assert_eq!(decoded[0], val);
    }

    #[test]
    fn test_encode_decode_string() {
        let val = Amf0Value::String("hello".to_string());
        let encoded = encode(&[val.clone()]);
        let (decoded, _) = decode(&encoded).unwrap();
        assert_eq!(decoded[0], val);
    }

    #[test]
    fn test_encode_decode_object() {
        let mut map = HashMap::new();
        map.insert("key".to_string(), Amf0Value::String("value".to_string()));
        map.insert("num".to_string(), Amf0Value::Number(1.0));
        let val = Amf0Value::Object(map);
        let encoded = encode(&[val.clone()]);
        let (decoded, _) = decode(&encoded).unwrap();
        assert!(matches!(&decoded[0], Amf0Value::Object(_)));
    }

    #[test]
    fn test_parse_command() {
        let values = vec![
            Amf0Value::String("connect".to_string()),
            Amf0Value::Number(1.0),
        ];
        let encoded = encode(&values);
        let (cmd, args) = parse_command(&encoded).unwrap();
        assert_eq!(cmd, "connect");
        assert_eq!(args.len(), 1);
    }
}
