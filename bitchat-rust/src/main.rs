use eframe::egui;
use tokio::sync::mpsc;
use std::collections::HashMap;

use crate::network::{NetworkManager, UiCommand, NetworkEvent, PeerMapKey};

pub mod protocol;
pub mod noise;
pub mod ble;
pub mod network;

#[tokio::main]
async fn main() -> Result<(), eframe::Error> {
    tracing_subscriber::fmt::init();

    // Create channels for communication between threads
    let (ui_cmd_tx, ui_cmd_rx) = mpsc::channel(32);
    let (net_event_tx, net_event_rx) = mpsc::channel(32);
    let (ble_cmd_tx, ble_cmd_rx) = mpsc::channel(32);
    let (ble_event_tx, ble_event_rx) = mpsc::channel(32);

    // Spawn the BLE task
    tokio::spawn(async move {
        if let Err(e) = ble::ble_task(ble_cmd_rx, ble_event_tx).await {
            eprintln!("BLE task failed: {}", e);
        }
    });

    // Spawn the NetworkManager task
    let mut network_manager = NetworkManager::new(ble_cmd_tx, net_event_tx, ui_cmd_rx, ble_event_rx);
    tokio::spawn(async move {
        network_manager.run().await;
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("BitChat Rust"),
        ..Default::default()
    };

    eframe::run_native(
        "bitchat_rust",
        options,
        Box::new(|_cc| Box::new(MyApp::new(ui_cmd_tx, net_event_rx))),
    )
}

struct PeerInfo {
    name: String,
    status: String,
}

struct MyApp {
    message_history: Vec<String>,
    input_text: String,
    peers: HashMap<PeerMapKey, PeerInfo>,
    status: String,
    ui_cmd_tx: mpsc::Sender<UiCommand>,
    net_event_rx: mpsc::Receiver<NetworkEvent>,
}

impl MyApp {
    fn new(ui_cmd_tx: mpsc::Sender<UiCommand>, net_event_rx: mpsc::Receiver<NetworkEvent>) -> Self {
        Self {
            message_history: vec!["Welcome to BitChat!".to_owned()],
            input_text: "".to_owned(),
            peers: HashMap::new(),
            status: "Initializing...".to_owned(),
            ui_cmd_tx,
            net_event_rx,
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check for events from the NetworkManager
        while let Ok(event) = self.net_event_rx.try_recv() {
            match event {
                NetworkEvent::PeerDiscovered { id, name } => {
                    self.peers.entry(id).or_insert(PeerInfo { name, status: "Discovered".to_string() });
                },
                NetworkEvent::PeerConnected(id) => {
                    if let Some(peer) = self.peers.get_mut(&id) {
                        peer.status = "Connected".to_string();
                    }
                },
                NetworkEvent::PeerDisconnected(id) => {
                     if let Some(peer) = self.peers.get_mut(&id) {
                        peer.status = "Disconnected".to_string();
                    }
                },
                NetworkEvent::NewMessage(msg) => {
                    self.message_history.push(msg);
                },
                NetworkEvent::StatusUpdate(status) => {
                    self.status = status;
                }
            }
        }

        // Status Bar
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.label(&self.status);
        });

        // Peer List
        egui::SidePanel::left("peer_list").show(ctx, |ui| {
            ui.heading("Peers");
            ui.separator();
            for (_id, peer) in &self.peers {
                ui.label(format!("{} ({})", peer.name, peer.status));
            }
        });

        // Main Chat Panel
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Chat Room");
            ui.separator();

            // Message History
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                for message in &self.message_history {
                    ui.label(message);
                }
            });

            // Separator and Input Box
            ui.separator();
            let text_edit = egui::TextEdit::singleline(&mut self.input_text)
                .hint_text("Type a message...");

            if ui.add(text_edit).lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if !self.input_text.is_empty() {
                    let text = self.input_text.clone();
                    self.ui_cmd_tx.try_send(UiCommand::SendMessage(text)).ok();
                    self.input_text.clear();
                }
            }
        });

        // Repaint continuously to check for new messages
        ctx.request_repaint();
    }
}
