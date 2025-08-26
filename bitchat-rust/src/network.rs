//! Manages the network layer, including peers, connections, and message routing.

use crate::compression;
use crate::noise::NoiseSession;
use crate::protocol::{BitchatMessage, BitchatPacket, FragmentPacket, MessageFlags, MessageType, self, AnnouncementPacket, PacketFlags};
use std::collections::{HashMap, HashSet};
use btleplug::platform::PeripheralId;
use sha2::{Sha256, Digest};
use tokio::sync::mpsc;
use tracing::{info, error, warn};
use uuid::Uuid;
use std::mem;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::identity::UserIdentity;
use ed25519_dalek::SigningKey;

const MAX_PAYLOAD_SIZE: usize = 480; // Max payload size before fragmentation

pub type PeerMapKey = PeripheralId;

pub enum PeerState {
    Discovered,
    Handshaking { noise: NoiseSession },
    Connected { noise: NoiseSession },
    Transitioning,
}

pub struct Peer {
    pub id: PeerMapKey,
    pub name: String,
    pub state: PeerState,
    pub bitchat_id: Option<[u8; 8]>,
    pub noise_public_key: Option<Vec<u8>>,
    pub signing_public_key: Option<Vec<u8>>,
}

pub struct NetworkManager {
    identity: UserIdentity,
    peers: HashMap<PeerMapKey, Peer>,
    seen_messages: HashSet<Uuid>,
    reassembly_buffers: HashMap<Uuid, HashMap<u16, Vec<u8>>>,
    ble_cmd_tx: mpsc::Sender<BleCommand>,
    net_event_tx: mpsc::Sender<NetworkEvent>,
    ui_cmd_rx: mpsc::Receiver<UiCommand>,
    ble_event_rx: mpsc::Receiver<BleEvent>,
}

impl NetworkManager {
    pub fn new(
        identity: UserIdentity,
        ble_cmd_tx: mpsc::Sender<BleCommand>,
        net_event_tx: mpsc::Sender<NetworkEvent>,
        ui_cmd_rx: mpsc::Receiver<UiCommand>,
        ble_event_rx: mpsc::Receiver<BleEvent>,
    ) -> Self {
        let self_bitchat_id = derive_bitchat_id(&identity.noise_keypair.public);
        info!("Our Bitchat ID for this session: {:?}", self_bitchat_id);

        Self {
            identity,
            peers: HashMap::new(),
            seen_messages: HashSet::new(),
            reassembly_buffers: HashMap::new(),
            ble_cmd_tx,
            net_event_tx,
            ui_cmd_rx,
            ble_event_rx,
        }
    }

    pub fn self_bitchat_id(&self) -> [u8; 8] {
        derive_bitchat_id(&self.identity.noise_keypair.public)
    }

    fn create_announcement_packet(&self) -> BitchatPacket {
        // Reconstruct the signing keypair from stored bytes
        let signing_key = SigningKey::from_keypair_bytes(&self.identity.signing_keypair.keypair_bytes)
            .expect("Failed to reconstruct signing key");

        let announce_payload = AnnouncementPacket {
            nickname: "bitchat-rust-user".to_string(), // Hardcoded for now
            noise_public_key: self.identity.noise_keypair.public.clone(),
            signing_public_key: signing_key.verifying_key().to_bytes().to_vec(),
        };

        let payload_bytes = announce_payload.to_bytes();

        BitchatPacket {
            version: 1,
            r#type: MessageType::Announce,
            ttl: 1, // Announce packets are not meant to be relayed
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
            flags: protocol::PacketFlags::empty(),
            sender_id: self.self_bitchat_id(),
            recipient_id: None, // Broadcast
            payload: payload_bytes,
            signature: None, // The announcement itself is not signed in the reference implementation
        }
    }

    pub async fn run(&mut self) {
        info!("NetworkManager running");
        loop {
            tokio::select! {
                Some(event) = self.ble_event_rx.recv() => {
                    self.handle_ble_event(event).await;
                },
                Some(command) = self.ui_cmd_rx.recv() => {
                    self.handle_ui_command(command).await;
                }
            }
        }
    }

    async fn handle_ble_event(&mut self, event: BleEvent) {
        match event {
            BleEvent::Discovered { id, name } => {
                if !self.peers.contains_key(&id) {
                    info!("Discovered new peer: {} ({:?})", name, id);
                    let peer = Peer {
                        id: id.clone(),
                        name: name.clone(),
                        state: PeerState::Discovered,
                        bitchat_id: None,
                        noise_public_key: None,
                        signing_public_key: None,
                    };
                    self.peers.insert(id.clone(), peer);
                    self.net_event_tx.send(NetworkEvent::PeerDiscovered { id: id.clone(), name }).await.ok();
                    self.ble_cmd_tx.send(BleCommand::Connect(id)).await.ok();
                }
            },
            BleEvent::ServicesDiscovered(id) => {
                info!("Services discovered for {:?}. Sending Announce packet.", id);
                let packet = self.create_announcement_packet();
                let data_to_send = packet.to_bytes();
                let ble_cmd_tx = self.ble_cmd_tx.clone();

                tokio::spawn(async move {
                    ble_cmd_tx.send(BleCommand::SendData { receiver: id, data: data_to_send }).await.ok();
                });
            },
            BleEvent::Connected(id) => {
                info!("Peer connected: {:?}. Waiting for service discovery.", id);
                self.net_event_tx.send(NetworkEvent::PeerConnected(id)).await.ok();
            },
            BleEvent::Disconnected(id) => {
                info!("Peer disconnected: {:?}", id);
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.state = PeerState::Discovered;
                }
                self.net_event_tx.send(NetworkEvent::PeerDisconnected(id)).await.ok();
            },
            BleEvent::DataReceived { sender, data } => {
                if let Some(mut peer) = self.peers.remove(&sender) {
                    let relay_packet = Self::handle_peer_data(
                        &mut peer,
                        &data,
                        self.ble_cmd_tx.clone(),
                        self.net_event_tx.clone(),
                        &self.identity,
                        &mut self.seen_messages,
                        &mut self.reassembly_buffers,
                    ).await;

                    let original_peer_id = peer.id.clone();
                    self.peers.insert(sender, peer);

                    if let Some(packet_to_relay) = relay_packet {
                        info!("Relaying message from {}", original_peer_id);
                        for (id, peer) in &self.peers {
                            if id != &original_peer_id && matches!(peer.state, PeerState::Connected { .. }) {
                                self.send_packet_fragmented(packet_to_relay.clone(), id.clone()).await;
                            }
                        }
                    }
                }
            }
        }
    }

    async fn handle_peer_data(
        peer: &mut Peer,
        data: &[u8],
        ble_cmd_tx: mpsc::Sender<BleCommand>,
        net_event_tx: mpsc::Sender<NetworkEvent>,
        identity: &UserIdentity,
        seen_messages: &mut HashSet<Uuid>,
        reassembly_buffers: &mut HashMap<Uuid, HashMap<u16, Vec<u8>>>,
    ) -> Option<BitchatPacket> {
        let packet = match BitchatPacket::from_bytes(data) {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to decode BitchatPacket from {}: {}", peer.name, e);
                return None;
            }
        };

        let self_bitchat_id = derive_bitchat_id(&identity.noise_keypair.public);
        let mut relay_packet = None;
        let old_state = mem::replace(&mut peer.state, PeerState::Transitioning);

        let new_state = match old_state {
            PeerState::Handshaking { mut noise } => {
                // ... (No relay logic during handshake)
                match packet.r#type {
                    MessageType::NoiseHandshake => {
                        match noise.handshake_read(&packet.payload) {
                            Ok(Some(reply_payload)) => {
                                let reply_packet = BitchatPacket {
                                    version: 1,
                                    r#type: MessageType::NoiseHandshake,
                                    ttl: 1,
                                    timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                                    flags: protocol::PacketFlags::empty(),
                                    sender_id: self_bitchat_id,
                                    recipient_id: None,
                                    payload: reply_payload,
                                    signature: None,
                                };
                                let data_to_send = reply_packet.to_bytes();
                                ble_cmd_tx.send(BleCommand::SendData { receiver: peer.id.clone(), data: data_to_send }).await.ok();

                                if noise.is_transport_mode() {
                                    info!("Handshake with {} complete!", peer.name);
                                    net_event_tx.send(NetworkEvent::PeerConnected(peer.id.clone())).await.ok();
                                    PeerState::Connected { noise }
                                } else {
                                    PeerState::Handshaking { noise }
                                }
                            },
                            Ok(None) => {
                                info!("Handshake with {} complete!", peer.name);
                                net_event_tx.send(NetworkEvent::PeerConnected(peer.id.clone())).await.ok();
                                PeerState::Connected { noise }
                            },
                            Err(e) => {
                                warn!("Handshake error with {}: {}", peer.name, e);
                                PeerState::Discovered
                            },
                        }
                    }
                    MessageType::Announce => {
                        info!("Received Announce from {} during handshake, processing it.", peer.name);
                        if let Ok(announce) = AnnouncementPacket::from_bytes(&packet.payload) {
                            peer.name = announce.nickname;
                            peer.noise_public_key = Some(announce.noise_public_key.clone());
                            peer.signing_public_key = Some(announce.signing_public_key);
                            peer.bitchat_id = Some(derive_bitchat_id(&announce.noise_public_key));
                            info!("Updated peer {} with bitchat_id {:?}", peer.name, peer.bitchat_id);
                        }
                        // Remain in the handshaking state
                        PeerState::Handshaking { noise }
                    }
                    _ => {
                        warn!("Received non-handshake packet of type {:?} during handshake from {}", packet.r#type, peer.name);
                        PeerState::Handshaking { noise }
                    }
                }
            },
            PeerState::Connected { mut noise } => {
                match packet.r#type {
                    MessageType::Announce => {
                        info!("Received Announce from {}", peer.name);
                        if let Ok(announce) = AnnouncementPacket::from_bytes(&packet.payload) {
                            peer.name = announce.nickname;
                            peer.noise_public_key = Some(announce.noise_public_key.clone());
                            peer.signing_public_key = Some(announce.signing_public_key);
                            peer.bitchat_id = Some(derive_bitchat_id(&announce.noise_public_key));
                            info!("Updated peer {} with bitchat_id {:?}", peer.name, peer.bitchat_id);
                        }
                    }
                    MessageType::Message => { // Unencrypted message - process but don't relay
                        if let Ok(message) = BitchatMessage::from_bytes(&packet.payload) {
                            if seen_messages.insert(message.id) {
                                net_event_tx.send(NetworkEvent::NewMessage {
                                    sender_id: peer.id.clone(),
                                    sender_name: peer.name.clone(),
                                    content: message.content,
                                    timestamp: packet.timestamp,
                                }).await.ok();
                            }
                        }
                    }
                    MessageType::NoiseEncrypted => {
                        match noise.decrypt(&packet.payload) {
                            Ok(plaintext) => {
                                if let Ok(inner_packet) = BitchatPacket::from_bytes(&plaintext) {
                                    if inner_packet.r#type == MessageType::Message {
                                        let message_payload_vec = if inner_packet.flags.contains(PacketFlags::IS_COMPRESSED) {
                                            match compression::decompress(&inner_packet.payload) {
                                                Ok(decompressed) => decompressed,
                                                Err(e) => {
                                                    warn!("Failed to decompress message from {}: {}", peer.name, e);
                                                    return None;
                                                }
                                            }
                                        } else {
                                            inner_packet.payload
                                        };

                                        if let Ok(message) = BitchatMessage::from_bytes(&message_payload_vec) {
                                            if seen_messages.insert(message.id) {
                                                info!("Received message {} from {}", message.id, peer.name);
                                                net_event_tx.send(NetworkEvent::NewMessage {
                                                    sender_id: peer.id.clone(),
                                                    sender_name: peer.name.clone(),
                                                    content: message.content,
                                                    timestamp: inner_packet.timestamp,
                                                }).await.ok();

                                                if inner_packet.ttl > 1 {
                                                    info!("Message has TTL {}, preparing to relay", inner_packet.ttl);
                                                    let mut relayed_outer = packet.clone();
                                                    relayed_outer.ttl -= 1;
                                                    relay_packet = Some(relayed_outer);
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            Err(e) => {
                                warn!("Failed to decrypt NoiseEncrypted message from {}: {}", peer.name, e);
                            }
                        }
                    }
                    MessageType::Fragment => {
                        if let Ok(fragment) = FragmentPacket::from_bytes(&packet.payload) {
                            let buffer = reassembly_buffers.entry(fragment.id).or_default();
                            buffer.insert(fragment.index, fragment.data);

                            if buffer.len() == fragment.count as usize {
                                info!("Reassembly complete for message {}", fragment.id);
                                let mut full_payload = Vec::new();
                                for i in 0..fragment.count {
                                    if let Some(chunk) = buffer.get(&i) {
                                        full_payload.extend_from_slice(chunk);
                                    } else {
                                        warn!("Missing fragment {} for message {}", i, fragment.id);
                                        reassembly_buffers.remove(&fragment.id);
                                        return None; // Reassembly failed
                                    }
                                }
                                reassembly_buffers.remove(&fragment.id);

                                // Recursively call handle_peer_data with the reassembled packet
                                // This is a bit tricky as we need to reconstruct a BitchatPacket
                                // from the payload. We assume the reassembled data is a complete BitchatPacket.
                                return Box::pin(Self::handle_peer_data(peer, &full_payload, ble_cmd_tx, net_event_tx, identity, seen_messages, reassembly_buffers)).await;
                            }
                        }
                    }
                    _ => {
                        warn!("Received unhandled packet type {:?} from {}", packet.r#type, peer.name);
                    }
                }
                PeerState::Connected { noise }
            },
            PeerState::Discovered => {
                // ... (No relay logic for discovered peers)
                match packet.r#type {
                    MessageType::Fragment => {
                        // Ignore fragments from discovered peers for now
                        warn!("Ignoring fragment from discovered peer {}", peer.name);
                        PeerState::Discovered
                    }
                    MessageType::Announce => {
                        info!("Received Announce from newly discovered peer {}", peer.name);
                        if let Ok(announce) = AnnouncementPacket::from_bytes(&packet.payload) {
                            peer.name = announce.nickname;
                            peer.noise_public_key = Some(announce.noise_public_key.clone());
                            peer.signing_public_key = Some(announce.signing_public_key);
                            peer.bitchat_id = Some(derive_bitchat_id(&announce.noise_public_key));
                            info!("Updated peer {} with bitchat_id {:?}", peer.name, peer.bitchat_id);
                        }

                        // Proactively initiate handshake after receiving announce
                        info!("Proactively initiating handshake with {}", peer.name);
                        match NoiseSession::new_initiator(&identity.noise_keypair.private) {
                            Ok((noise, init_msg)) => {
                                let packet = BitchatPacket {
                                    version: 1,
                                    r#type: MessageType::NoiseHandshake,
                                    ttl: 1,
                                    timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                                    flags: protocol::PacketFlags::empty(),
                                    sender_id: self_bitchat_id,
                                    recipient_id: None,
                                    payload: init_msg,
                                    signature: None,
                                };
                                let data_to_send = packet.to_bytes();
                                ble_cmd_tx.send(BleCommand::SendData { receiver: peer.id.clone(), data: data_to_send }).await.ok();
                                info!("Sent handshake initiation to {}", peer.name);
                                PeerState::Handshaking { noise }
                            }
                            Err(e) => {
                                error!("Failed to create initiator for {}: {}", peer.name, e);
                                PeerState::Discovered // Revert state
                            }
                        }
                    }
                    MessageType::NoiseHandshake => {
                        info!("Received handshake initiation from {}", peer.name);
                        match NoiseSession::new_responder(&identity.noise_keypair.private) {
                            Ok(mut noise) => {
                                match noise.handshake_read(&packet.payload) {
                                    Ok(Some(reply_payload)) => {
                                        let reply_packet = BitchatPacket {
                                            version: 1,
                                            r#type: MessageType::NoiseHandshake,
                                            ttl: 1,
                                            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                                            flags: protocol::PacketFlags::empty(),
                                            sender_id: self_bitchat_id,
                                            recipient_id: None,
                                            payload: reply_payload,
                                            signature: None,
                                        };
                                        let data_to_send = reply_packet.to_bytes();
                                        ble_cmd_tx.send(BleCommand::SendData { receiver: peer.id.clone(), data: data_to_send }).await.ok();
                                        info!("Sent handshake response to {}", peer.name);
                                        PeerState::Handshaking { noise }
                                    }
                                    Ok(None) => {
                                        // This shouldn't happen for a responder on the first message
                                        warn!("Handshake with {} completed prematurely.", peer.name);
                                        if noise.is_transport_mode() {
                                            PeerState::Connected { noise }
                                        } else {
                                            PeerState::Discovered
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Handshake error with {}: {}", peer.name, e);
                                        PeerState::Discovered
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to create responder for {}: {}", peer.name, e);
                                PeerState::Discovered
                            }
                        }
                    }
                    MessageType::Message => {
                        warn!("Received unencrypted Message from {}. This is deprecated. Initiating handshake.", peer.name);
                        // Be robust to clients that send a message to trigger a connection,
                        // which was a behaviour in early versions of the Swift client.
                        match NoiseSession::new_initiator(&identity.noise_keypair.private) {
                            Ok((noise, init_msg)) => {
                                let packet = BitchatPacket {
                                    version: 1,
                                    r#type: MessageType::NoiseHandshake,
                                    ttl: 1,
                                    timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                                    flags: protocol::PacketFlags::empty(),
                                    sender_id: self_bitchat_id,
                                    recipient_id: None,
                                    payload: init_msg,
                                    signature: None,
                                };
                                let data_to_send = packet.to_bytes();
                                ble_cmd_tx.send(BleCommand::SendData { receiver: peer.id.clone(), data: data_to_send }).await.ok();
                                info!("Sent handshake initiation to {}", peer.name);
                                PeerState::Handshaking { noise }
                            }
                            Err(e) => {
                                error!("Failed to create initiator for {}: {}", peer.name, e);
                                PeerState::Discovered // Revert state
                            }
                        }
                    }
                    _ => {
                        warn!("Received unexpected packet type {:?} from discovered peer {}", packet.r#type, peer.name);
                        PeerState::Discovered
                    }
                }
            }
            s => s, // Return original state for Transitioning
        };
        peer.state = new_state;
        relay_packet
    }

    async fn send_packet_fragmented(&self, packet: BitchatPacket, receiver: PeerMapKey) {
        let data_to_send = packet.to_bytes();
        if data_to_send.len() <= MAX_PAYLOAD_SIZE {
            // Packet is small enough, send as is
            let ble_cmd_tx = self.ble_cmd_tx.clone();
            tokio::spawn(async move {
                ble_cmd_tx.send(BleCommand::SendData { receiver, data: data_to_send }).await.ok();
            });
            return;
        }

        // Packet is too large, needs fragmentation
        info!("Packet too large ({} bytes), fragmenting.", data_to_send.len());
        let fragment_id = Uuid::new_v4();
        let chunks: Vec<_> = data_to_send.chunks(MAX_PAYLOAD_SIZE - 20).collect(); // -20 for fragment header
        let total_fragments = chunks.len() as u16;

        for (i, chunk) in chunks.iter().enumerate() {
            let fragment_packet = FragmentPacket {
                id: fragment_id,
                index: i as u16,
                count: total_fragments,
                data: chunk.to_vec(),
            };

            let bitchat_packet = BitchatPacket {
                version: 1,
                r#type: MessageType::Fragment,
                ttl: 1, // Fragments are not relayed
                timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                flags: protocol::PacketFlags::empty(),
                sender_id: self.self_bitchat_id(),
                recipient_id: None,
                payload: fragment_packet.to_bytes(),
                signature: None,
            };

            let ble_cmd_tx = self.ble_cmd_tx.clone();
            let receiver_clone = receiver.clone();
            let data_to_send = bitchat_packet.to_bytes();
            tokio::spawn(async move {
                ble_cmd_tx.send(BleCommand::SendData { receiver: receiver_clone, data: data_to_send }).await.ok();
            });
        }
    }

    async fn handle_ui_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::SendMessage(text) => {
                let self_bitchat_id = self.self_bitchat_id();
                let mut packets_to_send = Vec::new();

                for peer in self.peers.values_mut() {
                    if !matches!(peer.state, PeerState::Connected { .. }) {
                        continue; // Skip peers that are not fully connected
                    }

                    let old_state = mem::replace(&mut peer.state, PeerState::Transitioning);
                    let (new_state, packet_opt) = if let PeerState::Connected { mut noise } = old_state {
                        let packet = create_encrypted_message_packet(&mut noise, &text, self_bitchat_id, peer.bitchat_id);
                        (PeerState::Connected { noise }, packet)
                    } else {
                        (old_state, None) // Should not happen due to the check above
                    };

                    peer.state = new_state;
                    if let Some(packet) = packet_opt {
                        packets_to_send.push((packet, peer.id.clone()));
                    }
                }

                for (packet, receiver_id) in packets_to_send {
                    self.send_packet_fragmented(packet, receiver_id).await;
                }
            }
            UiCommand::SendPrivateMessage { recipient, text } => {
                let self_bitchat_id = self.self_bitchat_id();
                let packet_to_send = {
                    if let Some(peer) = self.peers.get_mut(&recipient) {
                        if let PeerState::Connected { ref mut noise } = peer.state {
                            create_encrypted_message_packet(noise, &text, self_bitchat_id, peer.bitchat_id)
                        } else {
                            warn!("Cannot send private message to {}: peer not connected.", peer.name);
                            None
                        }
                    } else {
                        warn!("Cannot send private message: recipient not found.");
                        None
                    }
                };

                if let Some(packet) = packet_to_send {
                    self.send_packet_fragmented(packet, recipient).await;
                }
            }
        }
    }
}

fn create_encrypted_message_packet(noise: &mut NoiseSession, text: &str, sender_id: [u8; 8], recipient_id: Option<[u8; 8]>) -> Option<BitchatPacket> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let message = BitchatMessage {
        flags: MessageFlags::empty(),
        timestamp: now,
        id: Uuid::new_v4(),
        sender: "Me".to_string(), // TODO: Use profile nickname
        content: text.to_string(),
        original_sender: None,
        recipient_nickname: None,
    };

    let message_bytes = message.to_bytes();
    let (payload, mut flags) = if message_bytes.len() > 64 { // Only compress if payload is somewhat large
        (compression::compress(&message_bytes), PacketFlags::IS_COMPRESSED)
    } else {
        (message_bytes, PacketFlags::empty())
    };

    if recipient_id.is_some() {
        flags |= PacketFlags::HAS_RECIPIENT;
    }

    let inner_packet = BitchatPacket {
        version: 1,
        r#type: MessageType::Message,
        ttl: if recipient_id.is_some() { 1 } else { 3 }, // Lower TTL for private messages
        timestamp: now,
        flags,
        sender_id,
        recipient_id,
        payload,
        signature: None,
    };

    if let Ok(encrypted_payload) = noise.encrypt(&inner_packet.to_bytes()) {
        let mut outer_flags = PacketFlags::empty();
        if recipient_id.is_some() {
            outer_flags |= PacketFlags::HAS_RECIPIENT;
        }
        Some(BitchatPacket {
            version: 1,
            r#type: MessageType::NoiseEncrypted,
            ttl: 1,
            timestamp: now,
            flags: outer_flags,
            sender_id,
            recipient_id,
            payload: encrypted_payload,
            signature: None,
        })
    } else {
        error!("Failed to encrypt message.");
        None
    }
}

fn derive_bitchat_id(public_key: &[u8]) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    let result = hasher.finalize();
    let mut id = [0u8; 8];
    id.copy_from_slice(&result[..8]);
    id
}

#[derive(Debug)]
pub enum UiCommand {
    SendMessage(String),
    SendPrivateMessage {
        recipient: PeerMapKey,
        text: String,
    },
}

#[derive(Debug, Clone)]
pub enum NetworkEvent {
    PeerDiscovered { id: PeerMapKey, name: String },
    PeerConnected(PeerMapKey),
    PeerDisconnected(PeerMapKey),
    NewMessage { sender_id: PeerMapKey, sender_name: String, content: String, timestamp: u64 },
    StatusUpdate(String),
}

#[derive(Debug)]
pub enum BleCommand {
    Connect(PeerMapKey),
    Disconnect(PeerMapKey),
    SendData {
        receiver: PeerMapKey,
        data: Vec<u8>,
    },
}

#[derive(Debug)]
pub enum BleEvent {
    Discovered { id: PeerMapKey, name: String },
    ServicesDiscovered(PeerMapKey),
    Connected(PeerMapKey),
    Disconnected(PeerMapKey),
    DataReceived { sender: PeerMapKey, data: Vec<u8> },
}
