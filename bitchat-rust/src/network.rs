//! Manages the network layer, including peers, connections, and message routing.

use crate::noise::NoiseSession;
use crate::protocol::{BitchatMessage, BitchatPacket, MessageFlags, MessageType, self, AnnouncementPacket};
use std::collections::HashMap;
use btleplug::platform::PeripheralId;
use sha2::{Sha256, Digest};
use tokio::sync::mpsc;
use tracing::{info, error, warn};
use uuid::Uuid;
use std::mem;
use std::time::{SystemTime, UNIX_EPOCH};
use rand::RngCore;

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
    self_id: [u8; 8],
    peers: HashMap<PeerMapKey, Peer>,
    ble_cmd_tx: mpsc::Sender<BleCommand>,
    net_event_tx: mpsc::Sender<NetworkEvent>,
    ui_cmd_rx: mpsc::Receiver<UiCommand>,
    ble_event_rx: mpsc::Receiver<BleEvent>,
}

impl NetworkManager {
    pub fn new(
        ble_cmd_tx: mpsc::Sender<BleCommand>,
        net_event_tx: mpsc::Sender<NetworkEvent>,
        ui_cmd_rx: mpsc::Receiver<UiCommand>,
        ble_event_rx: mpsc::Receiver<BleEvent>,
    ) -> Self {
        let mut self_id = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut self_id);
        info!("Our ID for this session: {:?}", self_id);

        Self {
            self_id,
            peers: HashMap::new(),
            ble_cmd_tx,
            net_event_tx,
            ui_cmd_rx,
            ble_event_rx,
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
                info!("Services discovered for {:?}. Ready for communication.", id);
                // Handshake is now lazy, initiated by sending a private message.
                // We should send an announce packet here to introduce ourselves.
                // TODO: Implement sending announce packets.
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
                if let Some(peer) = self.peers.get_mut(&sender) {
                    Self::handle_peer_data(peer, &data, &self.ble_cmd_tx, &self.net_event_tx, self.self_id).await;
                }
            }
        }
    }

    async fn handle_peer_data(
        peer: &mut Peer,
        data: &[u8],
        ble_cmd_tx: &mpsc::Sender<BleCommand>,
        net_event_tx: &mpsc::Sender<NetworkEvent>,
        self_id: [u8; 8],
    ) {
        let packet = match BitchatPacket::from_bytes(data) {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to decode BitchatPacket from {}: {}", peer.name, e);
                return;
            }
        };

        let old_state = mem::replace(&mut peer.state, PeerState::Transitioning);
        let new_state = match old_state {
            PeerState::Handshaking { mut noise } => {
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
                                    sender_id: self_id,
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
                    MessageType::Message => {
                        if let Ok(message) = BitchatMessage::from_bytes(&packet.payload) {
                            let display_msg = format!("{}: {}", message.sender, message.content);
                            net_event_tx.send(NetworkEvent::NewMessage(display_msg)).await.ok();
                        }
                    }
                    MessageType::NoiseEncrypted => {
                        match noise.decrypt(&packet.payload) {
                            Ok(plaintext) => {
                                info!("Decrypted a NoiseEncrypted packet from {}", peer.name);
                                // The plaintext should be another BitchatPacket
                                match BitchatPacket::from_bytes(&plaintext) {
                                    Ok(inner_packet) => {
                                        // For now, we only handle inner Message types
                                        if inner_packet.r#type == MessageType::Message {
                                            if let Ok(message) = BitchatMessage::from_bytes(&inner_packet.payload) {
                                                info!("Received message from {}: {}", peer.name, message.content);
                                                // Use the peer's name from the outer context, not the one from the message.
                                                let display_msg = format!("{}: {}", peer.name, message.content);
                                                net_event_tx.send(NetworkEvent::NewMessage(display_msg)).await.ok();
                                            } else {
                                                warn!("Failed to decode BitchatMessage from inner packet from {}", peer.name);
                                            }
                                        } else {
                                            warn!("Received unhandled inner packet type {:?} from {}", inner_packet.r#type, peer.name);
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to decode inner BitchatPacket from {}: {}", peer.name, e);
                                    }
                                }
                            },
                            Err(e) => {
                                warn!("Failed to decrypt NoiseEncrypted message from {}: {}", peer.name, e);
                            }
                        }
                    }
                    _ => {
                        warn!("Received unhandled packet type {:?} from {}", packet.r#type, peer.name);
                    }
                }
                PeerState::Connected { noise }
            },
            // If we are discovered, we shouldn't be receiving data yet. But if we do,
            // it could be an announce or a handshake initiation.
            PeerState::Discovered => {
                match packet.r#type {
                    MessageType::Announce => {
                        info!("Received Announce from newly discovered peer {}", peer.name);
                        if let Ok(announce) = AnnouncementPacket::from_bytes(&packet.payload) {
                            peer.name = announce.nickname;
                            peer.noise_public_key = Some(announce.noise_public_key.clone());
                            peer.signing_public_key = Some(announce.signing_public_key);
                            peer.bitchat_id = Some(derive_bitchat_id(&announce.noise_public_key));
                            info!("Updated peer {} with bitchat_id {:?}", peer.name, peer.bitchat_id);
                        }
                        PeerState::Discovered
                    }
                    MessageType::NoiseHandshake => {
                        info!("Received handshake initiation from {}", peer.name);
                        match NoiseSession::new_responder() {
                            Ok(mut noise) => {
                                match noise.handshake_read(&packet.payload) {
                                    Ok(Some(reply_payload)) => {
                                        let reply_packet = BitchatPacket {
                                            version: 1,
                                            r#type: MessageType::NoiseHandshake,
                                            ttl: 1,
                                            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                                            flags: protocol::PacketFlags::empty(),
                                            sender_id: self_id,
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
                        match NoiseSession::new_initiator() {
                            Ok((noise, init_msg)) => {
                                let packet = BitchatPacket {
                                    version: 1,
                                    r#type: MessageType::NoiseHandshake,
                                    ttl: 1,
                                    timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                                    flags: protocol::PacketFlags::empty(),
                                    sender_id: self_id,
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
    }

    async fn handle_ui_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::SendMessage(text) => {
                let mut commands_to_send = Vec::new();
                for peer in self.peers.values_mut() {
                    let old_state = mem::replace(&mut peer.state, PeerState::Transitioning);
                    let new_state = match old_state {
                        PeerState::Connected { mut noise } => {
                            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                            let message = BitchatMessage {
                                flags: MessageFlags::empty(),
                                timestamp: now,
                                id: Uuid::new_v4(),
                                sender: "Me".to_string(),
                                content: text.clone(),
                                original_sender: None,
                                recipient_nickname: None,
                            };
                            let inner_packet = BitchatPacket {
                                version: 1,
                                r#type: MessageType::Message,
                                ttl: 3,
                                timestamp: now,
                                flags: protocol::PacketFlags::empty(),
                                sender_id: self.self_id,
                                recipient_id: None,
                                payload: message.to_bytes(),
                                signature: None,
                            };

                            match noise.encrypt(&inner_packet.to_bytes()) {
                                Ok(encrypted_payload) => {
                                    let outer_packet = BitchatPacket {
                                        version: 1,
                                        r#type: MessageType::NoiseEncrypted,
                                        ttl: 1,
                                        timestamp: now,
                                        flags: protocol::PacketFlags::empty(),
                                        sender_id: self.self_id,
                                        recipient_id: peer.bitchat_id,
                                        payload: encrypted_payload,
                                        signature: None,
                                    };
                                    let data_to_send = outer_packet.to_bytes();
                                    info!("Queueing encrypted message to {}", peer.name);
                                    commands_to_send.push(BleCommand::SendData { receiver: peer.id.clone(), data: data_to_send });
                                }
                                Err(e) => {
                                    error!("Failed to encrypt message for {}: {}", peer.name, e);
                                }
                            }
                            PeerState::Connected { noise }
                        }
                        PeerState::Discovered => {
                            info!("Peer {} is discovered. Initiating handshake.", peer.name);
                            match NoiseSession::new_initiator() {
                                Ok((noise, init_msg)) => {
                                    let packet = BitchatPacket {
                                        version: 1,
                                        r#type: MessageType::NoiseHandshake,
                                        ttl: 1,
                                        timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                                        flags: protocol::PacketFlags::empty(),
                                        sender_id: self.self_id,
                                        recipient_id: None,
                                        payload: init_msg,
                                        signature: None,
                                    };
                                    let data_to_send = packet.to_bytes();
                                    info!("Queueing handshake initiation to {}. Please try sending message again later.", peer.name);
                                    commands_to_send.push(BleCommand::SendData { receiver: peer.id.clone(), data: data_to_send });
                                    PeerState::Handshaking { noise }
                                }
                                Err(e) => {
                                    error!("Failed to create initiator for {}: {}", peer.name, e);
                                    PeerState::Discovered // Revert state
                                }
                            }
                        }
                        other_state @ PeerState::Handshaking { .. } => {
                            info!("Already handshaking with {}. Please wait.", peer.name);
                            other_state
                        }
                        other_state => { // Catches Transitioning
                            warn!("Not sending message to {} because peer state is not ready (state: transitioning or other).", peer.name);
                            other_state
                        }
                    };
                    peer.state = new_state;
                }

                for cmd in commands_to_send {
                    self.ble_cmd_tx.send(cmd).await.ok();
                }
            }
        }
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
}

#[derive(Debug, Clone)]
pub enum NetworkEvent {
    PeerDiscovered { id: PeerMapKey, name: String },
    PeerConnected(PeerMapKey),
    PeerDisconnected(PeerMapKey),
    NewMessage(String),
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
