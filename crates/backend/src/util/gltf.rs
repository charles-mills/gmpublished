use serde_json::Value;

pub(crate) struct Document<'a> {
    pub(crate) json: Value,
    pub(crate) binary: Option<&'a [u8]>,
}

impl<'a> Document<'a> {
    pub(crate) fn parse(bytes: &'a [u8], binary: bool) -> Result<Self, String> {
        let mut blob = None;
        let json = if binary {
            if bytes.len() < 20
                || &bytes[..4] != b"glTF"
                || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != 2
                || &bytes[16..20] != b"JSON"
            {
                return Err("Invalid GLB JSON header".into());
            }
            if u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize != bytes.len() {
                return Err("GLB file length differs from its header".into());
            }
            let mut offset = 12_usize;
            let mut json = None;
            while offset + 8 <= bytes.len() {
                let length =
                    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                let kind = &bytes[offset + 4..offset + 8];
                offset += 8;
                let end = offset
                    .checked_add(length)
                    .filter(|end| *end <= bytes.len())
                    .ok_or("GLB chunk exceeds the file")?;
                match kind {
                    b"JSON" => json = Some(&bytes[offset..end]),
                    b"BIN\0" => blob = Some(&bytes[offset..end]),
                    _ => {}
                }
                offset = end;
            }
            json.ok_or("GLB JSON chunk is missing")?
        } else {
            bytes
        };
        Ok(Self {
            json: serde_json::from_slice(json).map_err(|error| error.to_string())?,
            binary: blob,
        })
    }

    pub(crate) fn external_uris(&self) -> impl Iterator<Item = &str> {
        ["buffers", "images"].into_iter().flat_map(|key| {
            self.json[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["uri"].as_str())
                .filter(|uri| !uri.starts_with("data:"))
        })
    }
}

pub(crate) fn decode_uri_path(uri: &str) -> Result<String, &'static str> {
    let mut decoded = Vec::with_capacity(uri.len());
    let mut bytes = uri.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|byte| char::from(byte).to_digit(16));
            let low = bytes.next().and_then(|byte| char::from(byte).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                return Err("Invalid percent escape in glTF URI");
            };
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).map_err(|_| "glTF URI is not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glb(json: &[u8], binary: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::from(&b"glTF"[..]);
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&(28_u32 + json.len() as u32 + binary.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"JSON");
        bytes.extend_from_slice(json);
        bytes.extend_from_slice(&(binary.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"BIN\0");
        bytes.extend_from_slice(binary);
        bytes
    }

    #[test]
    fn binary_and_text_documents_enumerate_the_same_external_dependencies() {
        let json = br#"{"buffers":[{"uri":"mesh%20data.bin"},{"uri":"data:;base64,AA=="}],"images":[{"uri":"textures/paint.png"},{"bufferView":0}]}"#;
        let bytes = glb(json, &[1, 2, 3, 4]);
        let binary = Document::parse(&bytes, true).unwrap();
        let text = Document::parse(json, false).unwrap();
        assert_eq!(binary.json, text.json);
        assert_eq!(binary.binary, Some(&[1, 2, 3, 4][..]));
        assert_eq!(
            binary.external_uris().collect::<Vec<_>>(),
            ["mesh%20data.bin", "textures/paint.png"]
        );
    }

    #[test]
    fn invalid_binary_headers_and_chunk_ranges_are_rejected() {
        let original = glb(b"{}", &[0; 4]);
        for offset in [0, 4, 8, 12, 16, 22] {
            let mut bytes = original.clone();
            bytes[offset] = 255;
            assert!(Document::parse(&bytes, true).is_err(), "offset {offset}");
        }
        for length in 0..original.len() {
            assert!(Document::parse(&original[..length], true).is_err());
        }
    }

    #[test]
    fn uri_decoding_preserves_unicode_and_rejects_invalid_escapes() {
        assert_eq!(decode_uri_path("a%20b/%E2%82%AC.bin").unwrap(), "a b/€.bin");
        for uri in ["%", "%1", "%GG", "%FF"] {
            assert!(decode_uri_path(uri).is_err());
        }
    }
}
