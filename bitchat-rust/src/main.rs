use eframe::egui::{self, Margin};
use tokio::sync::mpsc;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tracing::info;
use chrono::{DateTime, Utc};

use crate::network::{NetworkManager, UiCommand, NetworkEvent, PeerMapKey};
use crate::identity::UserIdentity;
use crate::command::{process_command, Command};

pub mod protocol;
pub mod noise;
pub mod ble;
pub mod network;
pub mod identity;
pub mod command;
pub mod compression;

#[cfg(feature = "geohash")]
pub mod geohash;

const IDENTITY_FILE: &str = "bitchat_identity.bin";

#[tokio::main]
async fn main() -> Result<(), eframe::Error> {
    tracing_subscriber::fmt::init();

    let identity = match UserIdentity::load_from_file(IDENTITY_FILE) {
        Ok(id) => {
            info!("Loaded identity from {}", IDENTITY_FILE);
            id
        },
        Err(_) => {
            info!("No identity file found, generating a new one...");
            let id = UserIdentity::generate().expect("Failed to generate identity");
            id.save_to_file(IDENTITY_FILE).expect("Failed to save identity file");
            info!("Saved new identity to {}", IDENTITY_FILE);
            id
        }
    };

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
    let mut network_manager = NetworkManager::new(identity, ble_cmd_tx, net_event_tx, ui_cmd_rx, ble_event_rx);
    tokio::spawn(async move {
        network_manager.run().await;
    });

    #[cfg(feature = "geohash")]
    {
        if let Ok(hash) = geohash::get_current_geohash() {
            println!("Current Geohash: {}", hash);
            // 将来的に、このハッシュをパケットに含める
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("BitChat Rust"),
        ..Default::default()
    };

    eframe::run_native(
        "bitchat_rust",
        options,
        Box::new(|cc| {
            let mut style = (*cc.egui_ctx.style()).clone();
            style.text_styles = [
                (egui::TextStyle::Heading, egui::FontId::new(20.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Body, egui::FontId::new(17.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Monospace, egui::FontId::new(14.0, egui::FontFamily::Monospace)),
                (egui::TextStyle::Button, egui::FontId::new(17.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Small, egui::FontId::new(15.0, egui::FontFamily::Proportional)),
            ]
            .into();

            let mut visuals = egui::Visuals::dark();
            visuals.window_fill = egui::Color32::from_hex("#1C1C1E").unwrap();
            visuals.widgets.noninteractive.bg_fill = egui::Color32::from_hex("#1C1C1E").unwrap();
            visuals.widgets.inactive.bg_fill = egui::Color32::from_hex("#2C2C2E").unwrap();
            visuals.widgets.hovered.bg_fill = egui::Color32::from_hex("#3A3A3C").unwrap();
            visuals.widgets.active.bg_fill = egui::Color32::from_hex("#3A3A3C").unwrap();
            visuals.override_text_color = Some(egui::Color32::from_hex("#FFFFFF").unwrap());
            visuals.hyperlink_color = egui::Color32::from_hex("#0A84FF").unwrap();
            visuals.selection.bg_fill = egui::Color32::from_hex("#0A84FF").unwrap();
            style.visuals = visuals;

            cc.egui_ctx.set_style(style);

            let mut fonts = egui::FontDefinitions::default();

            fonts.font_data.insert("Inter-Regular".to_owned(),
                egui::FontData::from_static(include_bytes!("assets/fonts/Inter-Regular.ttf")));
            fonts.font_data.insert("Inter-Bold".to_owned(),
                egui::FontData::from_static(include_bytes!("assets/fonts/Inter-Bold.ttf")));
            fonts.font_data.insert("Inter-Light".to_owned(),
                egui::FontData::from_static(include_bytes!("assets/fonts/Inter-Light.ttf")));

            fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap()
                .insert(0, "Inter-Regular".to_owned());
            fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap()
                .push("Inter-Bold".to_owned());
            fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap()
                .push("Inter-Light".to_owned());

            cc.egui_ctx.set_fonts(fonts);

            Box::new(MyApp::new(ui_cmd_tx, net_event_rx))
        }),
    )
}

#[derive(Clone)]
struct PeerInfo {
    name: String,
    status: String,
}

struct ChatMessage {
    is_self: bool,
    sender: String,
    content: String,
    timestamp: DateTime<Utc>,
}

struct MyApp {
    message_history: Vec<ChatMessage>,
    input_text: String,
    peers: HashMap<PeerMapKey, PeerInfo>,
    status: String,
    ui_cmd_tx: mpsc::Sender<UiCommand>,
    net_event_rx: mpsc::Receiver<NetworkEvent>,
    show_user_list: bool,
    username: String,
    private_chat_peer: Option<PeerMapKey>,
    active_chat_name: String,
    blocked_peers: HashSet<PeerMapKey>,
    favorite_peers: HashSet<PeerMapKey>,
    wipe_button_clicks: u8,
    last_wipe_click_time: Option<Instant>,
}

impl MyApp {
    fn new(ui_cmd_tx: mpsc::Sender<UiCommand>, net_event_rx: mpsc::Receiver<NetworkEvent>) -> Self {
        Self {
            message_history: vec![ChatMessage {
                is_self: false,
                sender: "System".to_string(),
                content: "Welcome to BitChat!".to_string(),
                timestamp: Utc::now(),
            }],
            input_text: "".to_owned(),
            peers: HashMap::new(),
            status: "Initializing...".to_owned(),
            ui_cmd_tx,
            net_event_rx,
            show_user_list: false,
            username: "user".to_owned(),
            private_chat_peer: None,
            active_chat_name: "#mesh".to_string(),
            blocked_peers: HashSet::new(),
            favorite_peers: HashSet::new(),
            wipe_button_clicks: 0,
            last_wipe_click_time: None,
        }
    }

    fn send_message(&mut self) {
        if self.input_text.is_empty() {
            return;
        }

        let text = self.input_text.trim().to_string();
        self.input_text.clear();

        if text.starts_with('/') {
            match process_command(&text) {
                Ok(command) => self.handle_command(command),
                Err(e) => self.add_system_message(e.to_string()),
            }
        } else {
            // Context-aware message sending
            if let Some(peer_id) = self.private_chat_peer.clone() {
                // We are in a private chat
                self.ui_cmd_tx.try_send(UiCommand::SendPrivateMessage { recipient: peer_id, text: text.clone() }).ok();
            } else {
                // We are in the public channel
                self.ui_cmd_tx.try_send(UiCommand::SendMessage(text.clone())).ok();
            }

            self.message_history.push(ChatMessage {
                is_self: true,
                sender: self.username.clone(),
                content: text,
                timestamp: Utc::now(),
            });
        }
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Help => {
                let help_text = "Commands:\n\
                                 /msg @name [message] - Send a private message\n\
                                 /who - List online users\n\
                                 /clear - Clear message history\n\
                                 /hug @name - Send a hug\n\
                                 /slap @name - Slap someone with a trout\n\
                                 /block @name - Block a user\n\
                                 /unblock @name - Unblock a user\n\
                                 /fav @name - Add user to favorites\n\
                                 /unfav @name - Remove user from favorites\n\
                                 /help - Show this help message";
                self.add_system_message(help_text.to_string());
            }
            Command::Who => {
                if self.peers.is_empty() {
                    self.add_system_message("No one else is online right now.".to_string());
                } else {
                    let peer_list = self.peers.values()
                        .map(|p| p.name.clone())
                        .collect::<Vec<String>>()
                        .join(", ");
                    self.add_system_message(format!("Online: {}", peer_list));
                }
            }
            Command::Msg { nickname, message } => {
                if nickname == "#mesh" {
                    self.private_chat_peer = None;
                    self.active_chat_name = "#mesh".to_string();
                    self.add_system_message("Joined #mesh".to_string());
                    return;
                }

                let target_peer_id = self.peers.iter().find(|(_, info)| info.name == nickname).map(|(id, _)| id.clone());

                if let Some(id) = target_peer_id {
                    self.private_chat_peer = Some(id.clone());
                    self.active_chat_name = format!("@{}", nickname);
                    self.add_system_message(format!("Started private chat with @{}", nickname));

                    if let Some(text) = message {
                        self.ui_cmd_tx.try_send(UiCommand::SendPrivateMessage { recipient: id, text: text.clone() }).ok();
                        self.message_history.push(ChatMessage {
                            is_self: true,
                            sender: self.username.clone(),
                            content: text,
                            timestamp: Utc::now(),
                        });
                    }
                } else {
                    self.add_system_message(format!("User '{}' not found.", nickname));
                }
            }
            Command::Clear => {
                self.message_history.clear();
            }
            Command::Hug { nickname } => {
                let message = format!("* 🫂 {} hugs {} *", self.username, nickname);
                // For now, this is a local message. Later it should be sent over the network.
                self.add_system_message(message);
            }
            Command::Slap { nickname } => {
                let message = format!("* 🐟 {} slaps {} around a bit with a large trout *", self.username, nickname);
                self.add_system_message(message);
            }
            Command::Block { nickname } => {
                if let Some((id, _)) = self.find_peer_by_name(&nickname) {
                    self.blocked_peers.insert(id);
                    self.add_system_message(format!("Blocked {}.", nickname));
                } else {
                    self.add_system_message(format!("User '{}' not found.", nickname));
                }
            }
            Command::Unblock { nickname } => {
                if let Some((id, _)) = self.find_peer_by_name(&nickname) {
                    if self.blocked_peers.remove(&id) {
                        self.add_system_message(format!("Unblocked {}.", nickname));
                    } else {
                        self.add_system_message(format!("{} was not blocked.", nickname));
                    }
                } else {
                    self.add_system_message(format!("User '{}' not found.", nickname));
                }
            }
            Command::Fav { nickname } => {
                if let Some((id, _)) = self.find_peer_by_name(&nickname) {
                    self.favorite_peers.insert(id);
                    self.add_system_message(format!("Added {} to favorites.", nickname));
                } else {
                    self.add_system_message(format!("User '{}' not found.", nickname));
                }
            }
            Command::Unfav { nickname } => {
                if let Some((id, _)) = self.find_peer_by_name(&nickname) {
                    if self.favorite_peers.remove(&id) {
                        self.add_system_message(format!("Removed {} from favorites.", nickname));
                    } else {
                        self.add_system_message(format!("{} was not a favorite.", nickname));
                    }
                } else {
                    self.add_system_message(format!("User '{}' not found.", nickname));
                }
            }
        }
    }

    fn find_peer_by_name(&self, nickname: &str) -> Option<(PeerMapKey, PeerInfo)> {
        self.peers.iter()
            .find(|(_, info)| info.name == nickname)
            .map(|(id, info)| (id.clone(), info.clone()))
    }

    fn wipe_data(&mut self) {
        // Clear all in-memory state
        self.message_history.clear();
        self.peers.clear();
        self.blocked_peers.clear();
        self.favorite_peers.clear();
        self.private_chat_peer = None;
        self.active_chat_name = "#mesh".to_string();
        self.status = "DATA WIPED".to_string();

        // Attempt to delete the identity file
        match std::fs::remove_file(IDENTITY_FILE) {
            Ok(_) => {
                self.add_system_message("Identity file wiped.".to_string());
            }
            Err(e) => {
                self.add_system_message(format!("Could not wipe identity file: {}", e));
            }
        }

        self.add_system_message("All data has been wiped. Please restart the application.".to_string());
    }

    fn add_system_message(&mut self, content: String) {
        self.message_history.push(ChatMessage {
            is_self: false,
            sender: "System".to_string(),
            content,
            timestamp: Utc::now(),
        });
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
                NetworkEvent::NewMessage { sender_id, sender_name, content, timestamp } => {
                    if !self.blocked_peers.contains(&sender_id) {
                        let dt = DateTime::from_timestamp_millis(timestamp as i64).unwrap_or_else(|| Utc::now());
                        self.message_history.push(ChatMessage {
                            is_self: false,
                            sender: sender_name,
                            content,
                            timestamp: dt,
                        });
                    }
                },
                NetworkEvent::StatusUpdate(status) => {
                    self.status = status;
                }
            }
        }

        // Header
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&self.active_chat_name).font(egui::FontId::proportional(17.0)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let users_button = ui.add(egui::Button::new(egui::RichText::new("👥").size(20.0)).frame(false));
                    if users_button.clicked() {
                        self.show_user_list = !self.show_user_list;
                    }
                });
            });
        });

        // User List (shown as a side panel when toggled)
        if self.show_user_list {
            egui::SidePanel::right("user_list").show(ctx, |ui| {
                ui.label(egui::RichText::new("Users").font(egui::FontId::proportional(15.0)).strong());
                ui.separator();

                // Username input
                ui.horizontal(|ui| {
                    ui.label("Username:");
                    ui.text_edit_singleline(&mut self.username);
                });

                ui.separator();

                for (_id, peer) in &self.peers {
                    ui.label(format!("{} ({})", peer.name, peer.status));
                }

                ui.separator();
                let wipe_button = ui.button("Wipe Data");
                if wipe_button.clicked() {
                    let now = Instant::now();
                    if let Some(last_click) = self.last_wipe_click_time {
                        if now.duration_since(last_click).as_secs_f32() < 1.5 {
                            self.wipe_button_clicks += 1;
                        } else {
                            self.wipe_button_clicks = 1;
                        }
                    } else {
                        self.wipe_button_clicks = 1;
                    }
                    self.last_wipe_click_time = Some(now);

                    if self.wipe_button_clicks >= 3 {
                        self.wipe_data();
                        self.wipe_button_clicks = 0;
                    } else {
                         self.add_system_message(format!("Click {} more times to wipe data.", 3 - self.wipe_button_clicks));
                    }
                }
            });
        }

        // Input area
        egui::TopBottomPanel::bottom("input_area").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let text_edit = egui::TextEdit::singleline(&mut self.input_text)
                    .hint_text("Type a message...")
                    .desired_width(f32::INFINITY);

                let response = ui.add(text_edit);

                let send_button = ui.add(egui::Button::new(egui::RichText::new("➤").size(20.0)).frame(false));
                if send_button.clicked() {
                    self.send_message();
                    response.request_focus();
                }

                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.send_message();
                    response.request_focus();
                }
            });
        });

        // Main Chat Panel
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    for msg in &self.message_history {
                        let layout = if msg.is_self {
                            egui::Layout::right_to_left(egui::Align::TOP).with_main_wrap(true)
                        } else {
                            egui::Layout::left_to_right(egui::Align::TOP).with_main_wrap(true)
                        };

                        ui.with_layout(layout, |ui| {
                            let bg_color = if msg.is_self {
                                egui::Color32::from_hex("#0A84FF").unwrap()
                            } else {
                                egui::Color32::from_hex("#3A3A3C").unwrap()
                            };

                            let frame = egui::Frame::default()
                                .inner_margin(Margin::same(8.0))
                                .rounding(egui::Rounding::same(12.0))
                                .fill(bg_color);

                            frame.show(ui, |ui| {
                                ui.set_max_width(300.0);
                                ui.label(&msg.content);
                            });

                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&msg.sender).strong());
                                ui.label(egui::RichText::new(msg.timestamp.format("%H:%M").to_string()).color(egui::Color32::GRAY));
                            });
                        });
                        ui.add_space(10.0);
                    }
                });
            });
        });

        // Repaint continuously to check for new messages
        ctx.request_repaint();
    }
}
