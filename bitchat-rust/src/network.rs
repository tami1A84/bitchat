//! Manages the network layer, including peers, connections, and message routing.

use crate::noise::NoiseSession;
use crate::protocol::{BitchatMessage, BitchatPacket, MessageFlags, MessageType, self};
use std::collections::HashMap;
use btleplug::platform::PeripheralId;
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
                    let peer = Peer { id: id.clone(), name: name.clone(), state: PeerState::Discovered };
                    self.peers.insert(id.clone(), peer);
                    self.net_event_tx.send(NetworkEvent::PeerDiscovered { id: id.clone(), name }).await.ok();
                    self.ble_cmd_tx.send(BleCommand::Connect(id)).await.ok();
                }
            },
            BleEvent::Connected(id) => {
                info!("Peer connected: {:?}. Initiating handshake.", id);
                if let Some(peer) = self.peers.get_mut(&id) {
                    match NoiseSession::new_initiator() {
                        Ok((noise, first_msg)) => {
                            peer.state = PeerState::Handshaking { noise };
                            self.ble_cmd_tx.send(BleCommand::SendData { receiver: id, data: first_msg }).await.ok();
                        },
                        Err(e) => error!("Failed to create initiator session: {}", e),
                    }
                }
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
                    Self::handle_peer_data(peer, &data, &self.ble_cmd_tx, &self.net_event_tx).await;
                }
            }
        }
    }

    async fn handle_peer_data(
        peer: &mut Peer,
        data: &[u8],
        ble_cmd_tx: &mpsc::Sender<BleCommand>,
        net_event_tx: &mpsc::Sender<NetworkEvent>,
    ) {
        let old_state = mem::replace(&mut peer.state, PeerState::Transitioning);
        let new_state = match old_state {
            PeerState::Handshaking { mut noise } => {
                match noise.handshake_read(data) {
                    Ok(Some(reply)) => {
                        ble_cmd_tx.send(BleCommand::SendData { receiver: peer.id.clone(), data: reply }).await.ok();
                        if noise.is_transport_mode() {
                            info!("Handshake with {} complete!", peer.name);
                            PeerState::Connected { noise }
                        } else {
                            PeerState::Handshaking { noise }
                        }
                    },
                    Ok(None) => {
                        info!("Handshake with {} complete!", peer.name);
                        PeerState::Connected { noise }
                    },
                    Err(e) => {
                        warn!("Handshake error with {}: {}", peer.name, e);
                        PeerState::Discovered
                    },
                }
            },
            PeerState::Connected { mut noise } => {
                match noise.decrypt(data) {
                    Ok(plaintext) => {
                        if let Ok(packet) = bincode::deserialize::<BitchatPacket>(&plaintext) {
                            if let Ok(message) = bincode::deserialize::<BitchatMessage>(&packet.payload) {
                                let display_msg = format!("{}: {}", message.sender, message.content);
                                net_event_tx.send(NetworkEvent::NewMessage(display_msg)).await.ok();
                            }
                        }
                        PeerState::Connected { noise }
                    },
                    Err(_) => {
                        warn!("Failed to decrypt message from {}", peer.name);
                        PeerState::Connected { noise }
                    }
                }
            },
            s => s, // Return original state if not handshaking or connected
        };
        peer.state = new_state;
    }

    async fn handle_ui_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::SendMessage(text) => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                let message = BitchatMessage {
                    flags: MessageFlags::empty(),
                    timestamp: now,
                    id: Uuid::new_v4(),
                    sender: "Me".to_string(), // A real app would have a configurable nickname
                    content: text,
                    original_sender: None,
                    recipient_nickname: None,
                };

                let packet = BitchatPacket {
                    version: 1,
                    r#type: MessageType::Message,
                    ttl: 3,
                    timestamp: now,
                    flags: protocol::PacketFlags::empty(),
                    sender_id: self.self_id,
                    recipient_id: None, // None for broadcast
                    payload: bincode::serialize(&message).unwrap(),
                    signature: None,
                };

                let serialized_packet = bincode::serialize(&packet).unwrap();

                for peer in self.peers.values_mut() {
                    if let PeerState::Connected { noise } = &mut peer.state {
                        match noise.encrypt(&serialized_packet) {
                            Ok(ciphertext) => {
                                let cmd = BleCommand::SendData { receiver: peer.id.clone(), data: ciphertext };
                                self.ble_cmd_tx.send(cmd).await.ok();
                            },
                            Err(_) => error!("Failed to encrypt message for {}", peer.name),
                        }
                    }
                }
            }
        }
    }
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
    Connected(PeerMapKey),
    Disconnected(PeerMapKey),
    DataReceived { sender: PeerMapKey, data: Vec<u8> },
}
