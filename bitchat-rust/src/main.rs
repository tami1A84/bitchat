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

struct PeerInfo {
    name: String,
    status: String,
}

struct MyApp {
    message_history: Vec<(Option<PeerMapKey>, String)>,
    input_text: String,
    peers: HashMap<PeerMapKey, PeerInfo>,
    status: String,
    ui_cmd_tx: mpsc::Sender<UiCommand>,
    net_event_rx: mpsc::Receiver<NetworkEvent>,
    show_user_list: bool,
    username: String,
}

impl MyApp {
    fn new(ui_cmd_tx: mpsc::Sender<UiCommand>, net_event_rx: mpsc::Receiver<NetworkEvent>) -> Self {
        Self {
            message_history: vec![(None, "Welcome to BitChat!".to_owned())],
            input_text: "".to_owned(),
            peers: HashMap::new(),
            status: "Initializing...".to_owned(),
            ui_cmd_tx,
            net_event_rx,
            show_user_list: false,
            username: "user".to_owned(),
        }
    }

    fn send_message(&mut self) {
        if !self.input_text.is_empty() {
            let text = self.input_text.clone();
            self.ui_cmd_tx.try_send(UiCommand::SendMessage(text.clone())).ok();
            self.message_history.push((None, text));
            self.input_text.clear();
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
                NetworkEvent::NewMessage { sender_id, content } => {
                    self.message_history.push((Some(sender_id), content));
                },
                NetworkEvent::StatusUpdate(status) => {
                    self.status = status;
                }
            }
        }

        // Header
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("#public").font(egui::FontId::proportional(17.0)).strong());
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
                for (sender_id, message) in &self.message_history {
                    let (is_self, nickname) = match sender_id {
                        Some(id) => (false, self.peers.get(id).map_or("unknown", |p| &p.name)),
                        None => (true, self.username.as_str()),
                    };

                    let layout = if is_self {
                        egui::Layout::right_to_left(egui::Align::TOP)
                    } else {
                        egui::Layout::left_to_right(egui::Align::TOP)
                    };

                    ui.with_layout(layout, |ui| {
                        ui.horizontal(|ui| {
                            // Timestamp
                            ui.label(egui::RichText::new("12:34").color(egui::Color32::from_rgb(142, 142, 147)));
                            // Nickname
                            ui.label(egui::RichText::new(nickname).strong());
                            // Message
                            ui.label(message);
                        });
                    });
                }
            });
        });

        // Repaint continuously to check for new messages
        ctx.request_repaint();
    }
}
