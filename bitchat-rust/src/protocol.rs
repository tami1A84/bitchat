use serde::{Serialize, Deserialize};
use bitflags::bitflags;
use uuid::Uuid;
use serde_with::serde_as;

bitflags! {
    #[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PacketFlags: u8 {
        const HAS_RECIPIENT = 1 << 0;
        const HAS_SIGNATURE = 1 << 1;
        const IS_COMPRESSED = 1 << 2;
    }
}

bitflags! {
    #[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MessageFlags: u8 {
        const IS_RELAY = 1 << 0;
        const IS_PRIVATE = 1 << 1;
        const HAS_ORIGINAL_SENDER = 1 << 2;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Message,
    DeliveryAck,
    ReadReceipt,
    NoiseHandshake,
    FragmentStart,
    FragmentContinue,
    FragmentEnd,
}

#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BitchatPacket {
    pub version: u8,
    pub r#type: MessageType,
    pub ttl: u8,
    pub timestamp: u64,
    pub flags: PacketFlags,
    pub sender_id: [u8; 8],
    pub recipient_id: Option<[u8; 8]>,
    pub payload: Vec<u8>,
    #[serde_as(as = "Option<serde_with::Bytes>")]
    pub signature: Option<[u8; 64]>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BitchatMessage {
    pub flags: MessageFlags,
    pub timestamp: u64,
    pub id: Uuid,
    pub sender: String,
    pub content: String,
    pub original_sender: Option<String>,
    pub recipient_nickname: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitchat_message_serialization() {
        let message = BitchatMessage {
            flags: MessageFlags::IS_PRIVATE,
            timestamp: 1234567890,
            id: Uuid::new_v4(),
            sender: "Alice".to_string(),
            content: "Hello Bob!".to_string(),
            original_sender: None,
            recipient_nickname: Some("Bob".to_string()),
        };

        let encoded = bincode::serialize(&message).unwrap();
        let decoded: BitchatMessage = bincode::deserialize(&encoded).unwrap();

        assert_eq!(message, decoded);
    }

    #[test]
    fn test_bitchat_packet_serialization() {
        let message = BitchatMessage {
            flags: MessageFlags::IS_PRIVATE,
            timestamp: 1234567890,
            id: Uuid::new_v4(),
            sender: "Alice".to_string(),
            content: "Hello Bob!".to_string(),
            original_sender: None,
            recipient_nickname: Some("Bob".to_string()),
        };
        let payload = bincode::serialize(&message).unwrap();

        let packet = BitchatPacket {
            version: 1,
            r#type: MessageType::Message,
            ttl: 3,
            timestamp: 9876543210,
            flags: PacketFlags::HAS_RECIPIENT.union(PacketFlags::HAS_SIGNATURE),
            sender_id: [1; 8],
            recipient_id: Some([2; 8]),
            payload,
            signature: Some([3; 64]),
        };

        let encoded = bincode::serialize(&packet).unwrap();
        let decoded: BitchatPacket = bincode::deserialize(&encoded).unwrap();

        assert_eq!(packet, decoded);
        assert_eq!(packet.recipient_id, Some([2; 8]));
        assert_eq!(packet.signature, Some([3; 64]));


        // Test without optional fields
        let packet_broadcast = BitchatPacket {
            flags: PacketFlags::empty(),
            recipient_id: None,
            signature: None,
            ..packet.clone()
        };

        let encoded_broadcast = bincode::serialize(&packet_broadcast).unwrap();
        let decoded_broadcast: BitchatPacket = bincode::deserialize(&encoded_broadcast).unwrap();
        assert_eq!(packet_broadcast, decoded_broadcast);
        assert_eq!(decoded_broadcast.recipient_id, None);
        assert_eq!(decoded_broadcast.signature, None);
    }
}
