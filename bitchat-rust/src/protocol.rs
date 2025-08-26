use bitflags::bitflags;
use uuid::Uuid;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PacketFlags: u8 {
        const HAS_RECIPIENT = 1 << 0;
        const HAS_SIGNATURE = 1 << 1;
        const IS_COMPRESSED = 1 << 2;
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MessageFlags: u8 {
        const IS_RELAY = 1 << 0;
        const IS_PRIVATE = 1 << 1;
        const HAS_ORIGINAL_SENDER = 1 << 2;
        const HAS_RECIPIENT_NICKNAME = 1 << 3;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Announce = 0x01,
    Message = 0x02,
    Leave = 0x03,
    NoiseHandshake = 0x10,
    NoiseEncrypted = 0x11,
    Fragment = 0x20,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BitchatPacket {
    pub version: u8,
    pub r#type: MessageType,
    pub ttl: u8,
    pub timestamp: u64,
    pub flags: PacketFlags,
    pub sender_id: [u8; 8],
    pub recipient_id: Option<[u8; 8]>,
    pub payload: Vec<u8>,
    pub signature: Option<[u8; 64]>,
}

impl BitchatPacket {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Header (13 bytes)
        // Version
        data.push(self.version);
        // Type
        data.push(self.r#type.clone() as u8);
        // TTL
        data.push(self.ttl);
        // Timestamp (8 bytes, big-endian)
        data.extend_from_slice(&self.timestamp.to_be_bytes());
        // Flags (1 byte)
        // let mut flags = PacketFlags::empty();
        // if self.recipient_id.is_some() {
        //     flags |= PacketFlags::HAS_RECIPIENT;
        // }
        // if self.signature.is_some() {
        //     flags |= PacketFlags::HAS_SIGNATURE;
        // }
        data.push(self.flags.bits());
        // Payload length (2 bytes, big-endian)
        let payload_length = self.payload.len() as u16;
        data.extend_from_slice(&payload_length.to_be_bytes());

        // Variable sections
        // SenderID (8 bytes)
        data.extend_from_slice(&self.sender_id);

        // RecipientID (if present)
        if let Some(recipient_id) = self.recipient_id {
            data.extend_from_slice(&recipient_id);
        }

        // Payload
        data.extend_from_slice(&self.payload);

        // Signature (if present)
        if let Some(signature) = self.signature {
            data.extend_from_slice(&signature);
        }

        data
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        const HEADER_SIZE: usize = 13;
        const SENDER_ID_SIZE: usize = 8;
        const RECIPIENT_ID_SIZE: usize = 8;
        const SIGNATURE_SIZE: usize = 64;

        if data.len() < HEADER_SIZE {
            return Err("Data too short for header".to_string());
        }

        let mut offset = 0;

        let version = data[offset];
        offset += 1;
        if version != 1 {
            return Err(format!("Unsupported protocol version: {}", version));
        }

        let type_byte = data[offset];
        let r#type = match type_byte {
            0x01 => MessageType::Announce,
            0x02 => MessageType::Message,
            0x03 => MessageType::Leave,
            0x10 => MessageType::NoiseHandshake,
            0x11 => MessageType::NoiseEncrypted,
            0x20 => MessageType::Fragment,
            _ => return Err(format!("Unknown message type: {}", type_byte)),
        };
        offset += 1;

        let ttl = data[offset];
        offset += 1;

        let timestamp = u64::from_be_bytes(data[offset..offset+8].try_into().map_err(|e| format!("Error parsing timestamp: {:?}", e))?);
        offset += 8;

        let flags = PacketFlags::from_bits_truncate(data[offset]);
        offset += 1;

        let payload_length = u16::from_be_bytes(data[offset..offset+2].try_into().map_err(|e| format!("Error parsing payload length: {:?}", e))?) as usize;
        offset += 2;

        if data.len() < offset + SENDER_ID_SIZE {
            return Err("Data too short for sender ID".to_string());
        }
        let sender_id: [u8; 8] = data[offset..offset+SENDER_ID_SIZE].try_into().map_err(|e| format!("Error parsing sender_id: {:?}", e))?;
        offset += SENDER_ID_SIZE;

        let mut recipient_id: Option<[u8; 8]> = None;
        if flags.contains(PacketFlags::HAS_RECIPIENT) {
            if data.len() < offset + RECIPIENT_ID_SIZE {
                return Err("Data too short for recipient ID".to_string());
            }
            recipient_id = Some(data[offset..offset+RECIPIENT_ID_SIZE].try_into().map_err(|e| format!("Error parsing recipient_id: {:?}", e))?);
            offset += RECIPIENT_ID_SIZE;
        }

        if data.len() < offset + payload_length {
            return Err("Data too short for payload".to_string());
        }
        let payload = data[offset..offset+payload_length].to_vec();
        offset += payload_length;

        let mut signature: Option<[u8; 64]> = None;
        if flags.contains(PacketFlags::HAS_SIGNATURE) {
            if data.len() < offset + SIGNATURE_SIZE {
                return Err("Data too short for signature".to_string());
            }
            signature = Some(data[offset..offset+SIGNATURE_SIZE].try_into().map_err(|e| format!("Error parsing signature: {:?}", e))?);
            // offset += SIGNATURE_SIZE; // This was the last read, offset is not used anymore
        }

        // Note: This does not account for padding. The Swift client adds padding,
        // so a check like `data.len() != offset` would fail.
        // We are parsing based on the specified lengths, and ignoring any trailing bytes.

        Ok(BitchatPacket {
            version,
            r#type,
            ttl,
            timestamp,
            flags,
            sender_id,
            recipient_id,
            payload,
            signature,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BitchatMessage {
    pub flags: MessageFlags,
    pub timestamp: u64,
    pub id: Uuid,
    pub sender: String,
    pub content: String,
    pub original_sender: Option<String>,
    pub recipient_nickname: Option<String>,
}

impl BitchatMessage {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Flags
        data.push(self.flags.bits());

        // Timestamp (milliseconds)
        data.extend_from_slice(&self.timestamp.to_be_bytes());

        // ID
        let id_bytes = self.id.to_string().into_bytes();
        // Guard against oversized string
        if id_bytes.len() > 255 {
            // TODO: maybe return an error? For now, truncate.
            data.push(255);
            data.extend_from_slice(&id_bytes[..255]);
        } else {
            data.push(id_bytes.len() as u8);
            data.extend_from_slice(&id_bytes);
        }

        // Sender
        let sender_bytes = self.sender.as_bytes();
        if sender_bytes.len() > 255 {
            data.push(255);
            data.extend_from_slice(&sender_bytes[..255]);
        } else {
            data.push(sender_bytes.len() as u8);
            data.extend_from_slice(sender_bytes);
        }

        // Content
        let content_bytes = self.content.as_bytes();
        let content_len = content_bytes.len() as u16;
        data.extend_from_slice(&content_len.to_be_bytes());
        data.extend_from_slice(content_bytes);

        // Optional fields
        if let Some(original_sender) = &self.original_sender {
            let bytes = original_sender.as_bytes();
            if bytes.len() > 255 {
                data.push(255);
                data.extend_from_slice(&bytes[..255]);
            } else {
                data.push(bytes.len() as u8);
                data.extend_from_slice(bytes);
            }
        }

        if let Some(recipient_nickname) = &self.recipient_nickname {
            let bytes = recipient_nickname.as_bytes();
            if bytes.len() > 255 {
                data.push(255);
                data.extend_from_slice(&bytes[..255]);
            } else {
                data.push(bytes.len() as u8);
                data.extend_from_slice(bytes);
            }
        }

        data
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.is_empty() {
            return Err("Cannot decode empty message".to_string());
        }
        let mut offset = 0;

        // Flags
        let flags = MessageFlags::from_bits_truncate(data[offset]);
        offset += 1;

        // Timestamp
        if data.len() < offset + 8 { return Err("Data too short for timestamp".to_string()); }
        let timestamp = u64::from_be_bytes(data[offset..offset+8].try_into().map_err(|e| format!("Error parsing timestamp: {:?}", e))?);
        offset += 8;

        // ID
        if data.len() < offset + 1 { return Err("Data too short for ID length".to_string()); }
        let id_len = data[offset] as usize;
        offset += 1;
        if data.len() < offset + id_len { return Err("Data too short for ID".to_string()); }
        let id_str = String::from_utf8_lossy(&data[offset..offset+id_len]);
        let id = Uuid::parse_str(&id_str).map_err(|e| format!("UUID parse error: {}", e))?;
        offset += id_len;

        // Sender
        if data.len() < offset + 1 { return Err("Data too short for sender length".to_string()); }
        let sender_len = data[offset] as usize;
        offset += 1;
        if data.len() < offset + sender_len { return Err("Data too short for sender".to_string()); }
        let sender = String::from_utf8_lossy(&data[offset..offset+sender_len]).to_string();
        offset += sender_len;

        // Content
        if data.len() < offset + 2 { return Err("Data too short for content length".to_string()); }
        let content_len = u16::from_be_bytes(data[offset..offset+2].try_into().map_err(|e| format!("Error parsing content length: {:?}", e))?) as usize;
        offset += 2;
        if data.len() < offset + content_len { return Err("Data too short for content".to_string()); }
        let content = String::from_utf8_lossy(&data[offset..offset+content_len]).to_string();
        offset += content_len;

        // Optional fields
        let mut original_sender: Option<String> = None;
        if flags.contains(MessageFlags::HAS_ORIGINAL_SENDER) {
            if data.len() < offset + 1 { return Err("Data too short for original_sender length".to_string()); }
            let len = data[offset] as usize;
            offset += 1;
            if data.len() < offset + len { return Err("Data too short for original_sender".to_string()); }
            original_sender = Some(String::from_utf8_lossy(&data[offset..offset+len]).to_string());
            offset += len;
        }

        let mut recipient_nickname: Option<String> = None;
        if flags.contains(MessageFlags::HAS_RECIPIENT_NICKNAME) {
            if data.len() < offset + 1 { return Err("Data too short for recipient_nickname length".to_string()); }
            let len = data[offset] as usize;
            offset += 1;
            if data.len() < offset + len { return Err("Data too short for recipient_nickname".to_string()); }
            recipient_nickname = Some(String::from_utf8_lossy(&data[offset..offset+len]).to_string());
            // offset += len; // This was the last read, offset is not used anymore
        }

        Ok(BitchatMessage {
            flags,
            timestamp,
            id,
            sender,
            content,
            original_sender,
            recipient_nickname,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fragment_packet_roundtrip() {
        let packet = FragmentPacket {
            id: Uuid::new_v4(),
            index: 1,
            count: 5,
            data: vec![1, 2, 3, 4, 5],
        };

        let encoded = packet.to_bytes();
        let decoded = FragmentPacket::from_bytes(&encoded).unwrap();

        assert_eq!(packet, decoded);
    }

    #[test]
    fn test_announce_packet_roundtrip() {
        let packet = AnnouncementPacket {
            nickname: "Alice".to_string(),
            noise_public_key: vec![1; 32],
            signing_public_key: vec![2; 32],
        };

        let encoded = packet.to_bytes();
        let decoded = AnnouncementPacket::from_bytes(&encoded).unwrap();

        assert_eq!(packet, decoded);
    }

    #[test]
    fn test_bitchat_message_roundtrip() {
        let message = BitchatMessage {
            flags: MessageFlags::IS_PRIVATE | MessageFlags::HAS_RECIPIENT_NICKNAME,
            timestamp: 1234567890,
            id: Uuid::new_v4(),
            sender: "Alice".to_string(),
            content: "Hello Bob!".to_string(),
            original_sender: None,
            recipient_nickname: Some("Bob".to_string()),
        };

        let encoded = message.to_bytes();
        let decoded = BitchatMessage::from_bytes(&encoded).unwrap();

        assert_eq!(message, decoded);
    }

    #[test]
    fn test_bitchat_packet_roundtrip() {
        let message = BitchatMessage {
            flags: MessageFlags::IS_PRIVATE,
            timestamp: 1234567890,
            id: Uuid::new_v4(),
            sender: "Alice".to_string(),
            content: "Hello Bob!".to_string(),
            original_sender: None,
            recipient_nickname: Some("Bob".to_string()),
        };
        let payload = message.to_bytes();

        let packet = BitchatPacket {
            version: 1,
            r#type: MessageType::Message,
            ttl: 3,
            timestamp: 9876543210,
            flags: PacketFlags::HAS_RECIPIENT | PacketFlags::HAS_SIGNATURE,
            sender_id: [1; 8],
            recipient_id: Some([2; 8]),
            payload,
            signature: Some([3; 64]),
        };

        let encoded = packet.to_bytes();
        let decoded = BitchatPacket::from_bytes(&encoded).unwrap();

        assert_eq!(packet, decoded);
    }

    #[test]
    fn test_bitchat_packet_broadcast_roundtrip() {
        let packet = BitchatPacket {
            version: 1,
            r#type: MessageType::Message,
            ttl: 7,
            timestamp: 1111111111,
            flags: PacketFlags::empty(),
            sender_id: [9; 8],
            recipient_id: None,
            payload: vec![1, 2, 3, 4, 5],
            signature: None,
        };

        let encoded = packet.to_bytes();
        let decoded = BitchatPacket::from_bytes(&encoded).unwrap();

        assert_eq!(packet, decoded);
    }
}


#[derive(Debug, Clone, PartialEq)]
pub struct FragmentPacket {
    pub id: Uuid,
    pub index: u16,
    pub count: u16,
    pub data: Vec<u8>,
}

impl FragmentPacket {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.id.as_bytes());
        bytes.extend_from_slice(&self.index.to_be_bytes());
        bytes.extend_from_slice(&self.count.to_be_bytes());
        bytes.extend_from_slice(&self.data);
        bytes
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < 20 { // 16 for Uuid + 2 for index + 2 for count
            return Err("Data too short for FragmentPacket".to_string());
        }
        let id = Uuid::from_slice(&data[0..16]).map_err(|e| e.to_string())?;
        let index = u16::from_be_bytes(data[16..18].try_into().unwrap());
        let count = u16::from_be_bytes(data[18..20].try_into().unwrap());
        let fragment_data = data[20..].to_vec();

        Ok(FragmentPacket {
            id,
            index,
            count,
            data: fragment_data,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnnouncementPacket {
    pub nickname: String,
    pub noise_public_key: Vec<u8>,
    pub signing_public_key: Vec<u8>,
}

impl AnnouncementPacket {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Nickname (Type 0x01)
        if !self.nickname.is_empty() {
            data.push(0x01);
            let nickname_bytes = self.nickname.as_bytes();
            data.push(nickname_bytes.len() as u8); // Assuming length fits in a u8
            data.extend_from_slice(nickname_bytes);
        }

        // Noise Public Key (Type 0x02)
        data.push(0x02);
        data.push(self.noise_public_key.len() as u8);
        data.extend_from_slice(&self.noise_public_key);

        // Signing Public Key (Type 0x03)
        data.push(0x03);
        data.push(self.signing_public_key.len() as u8);
        data.extend_from_slice(&self.signing_public_key);

        data
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        let mut offset = 0;
        let mut nickname: Option<String> = None;
        let mut noise_public_key: Option<Vec<u8>> = None;
        let mut signing_public_key: Option<Vec<u8>> = None;

        while offset + 2 <= data.len() {
            let t = data[offset];
            offset += 1;
            let l = data[offset] as usize;
            offset += 1;

            if offset + l > data.len() {
                return Err("Invalid TLV length".to_string());
            }
            let v = &data[offset..offset + l];
            offset += l;

            match t {
                0x01 => nickname = Some(String::from_utf8_lossy(v).to_string()),
                0x02 => noise_public_key = Some(v.to_vec()),
                0x03 => signing_public_key = Some(v.to_vec()),
                _ => { /* Ignore unknown TLV types */ }
            }
        }

        if let (Some(nickname), Some(noise_public_key), Some(signing_public_key)) = (nickname, noise_public_key, signing_public_key) {
            Ok(AnnouncementPacket {
                nickname,
                noise_public_key,
                signing_public_key,
            })
        } else {
            Err("Missing required fields in AnnouncementPacket".to_string())
        }
    }
}
