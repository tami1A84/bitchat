//! Bluetooth Low Energy (BLE) communication task.

use btleplug::api::{Central, Manager as _, ScanFilter, Peripheral as _, WriteType, CentralEvent, CharPropFlags};
use btleplug::platform::{Manager, Peripheral};
use tokio::sync::mpsc;
use futures::stream::StreamExt;
use tracing::{info, error, warn};
use std::collections::HashMap;
use uuid::Uuid;
use crate::network::{BleCommand, BleEvent};

// UUIDs from the iOS/Android clients
const SERVICE_UUID: Uuid = Uuid::from_u128(0xF47B5E2D_4A9E_4C5A_9B3F_8E1D2C3A4B5C);
const CHARACTERISTIC_UUID: Uuid = Uuid::from_u128(0xA1B2C3D4_E5F6_4A5B_8C9D_0E1F2A3B4C5D);

/// Spawns a dedicated task to handle notifications from a single peripheral.
async fn spawn_notification_handler(
    peripheral: Peripheral,
    event_tx: mpsc::Sender<BleEvent>,
) {
    let peripheral_id = peripheral.id();
    info!("Starting notification handler for {:?}", peripheral_id);

    if let Err(e) = peripheral.discover_services().await {
        error!("[{:?}] Failed to discover services: {}", peripheral_id, e);
        return;
    }

    let chat_char = peripheral.characteristics().into_iter().find(|c| c.uuid == CHARACTERISTIC_UUID);

    if let Some(c) = chat_char {
        // Signal that we found the service and characteristic
        event_tx.send(BleEvent::ServicesDiscovered(peripheral.id())).await.ok();

        // Correctly check characteristic properties
        if c.properties.contains(CharPropFlags::NOTIFY) {
            if let Err(e) = peripheral.subscribe(&c).await {
                error!("[{:?}] Failed to subscribe to characteristic: {}", peripheral_id, e);
                return;
            }
            info!("[{:?}] Subscribed to Bitchat characteristic", peripheral_id);
        } else {
            warn!("[{:?}] Bitchat characteristic does not support notifications", peripheral_id);
            return;
        }
    } else {
        warn!("[{:?}] Bitchat characteristic not found", peripheral_id);
        return;
    }

    let mut notification_stream = match peripheral.notifications().await {
        Ok(stream) => stream,
        Err(e) => {
            error!("[{:?}] Failed to get notification stream: {}", peripheral_id, e);
            return;
        }
    };

    while let Some(notification) = notification_stream.next().await {
        if notification.uuid == CHARACTERISTIC_UUID {
            event_tx.send(BleEvent::DataReceived {
                sender: peripheral_id.clone(),
                data: notification.value,
            }).await.ok();
        }
    }

    info!("Notification handler for {:?} finished.", peripheral_id);
}


pub async fn ble_task(
    mut cmd_rx: mpsc::Receiver<BleCommand>,
    event_tx: mpsc::Sender<BleEvent>,
) -> Result<(), String> {
    let manager = Manager::new().await.map_err(|e| e.to_string())?;
    let adapters = manager.adapters().await.map_err(|e| e.to_string())?;
    let central = adapters.into_iter().next()
        .ok_or_else(|| "No BLE adapters found.".to_string())?;

    let mut peripherals: HashMap<btleplug::platform::PeripheralId, Peripheral> = HashMap::new();

    let filter = ScanFilter {
        services: vec![SERVICE_UUID],
    };
    central.start_scan(filter).await.map_err(|e| e.to_string())?;
    info!("Scanning for Bitchat devices...");

    let mut central_events = central.events().await.map_err(|e| e.to_string())?;

    loop {
        tokio::select! {
            Some(event) = central_events.next() => {
                match event {
                    CentralEvent::DeviceDiscovered(id) => {
                        if let Ok(p) = central.peripheral(&id).await {
                            let props = p.properties().await.unwrap_or(None);
                            let name = props.and_then(|p| p.local_name).unwrap_or_else(|| "Bitchat Peer".to_string());

                            info!("Discovered device: {} ({:?})", name, id);
                            peripherals.insert(id.clone(), p);
                            event_tx.send(BleEvent::Discovered { id, name }).await.ok();
                        }
                    },
                    CentralEvent::DeviceConnected(id) => {
                        info!("Device connected: {:?}", id);
                        if let Some(p) = peripherals.get(&id) {
                            tokio::spawn(spawn_notification_handler(p.clone(), event_tx.clone()));
                        }
                        event_tx.send(BleEvent::Connected(id)).await.ok();
                    },
                    CentralEvent::DeviceDisconnected(id) => {
                        info!("Device disconnected: {:?}", id);
                        peripherals.remove(&id);
                        event_tx.send(BleEvent::Disconnected(id)).await.ok();
                    },
                    _ => {}
                }
            },
            Some(command) = cmd_rx.recv() => {
                match command {
                    BleCommand::Connect(id) => {
                        if let Some(p) = peripherals.get(&id) {
                            info!("Connecting to {:?}...", id);
                            if let Err(e) = p.connect().await {
                                error!("Failed to connect to {:?}: {}", id, e);
                            }
                        }
                    },
                    BleCommand::Disconnect(id) => {
                         if let Some(p) = peripherals.get(&id) {
                            info!("Disconnecting from {:?}...", id);
                            p.disconnect().await.ok();
                        }
                    },
                    BleCommand::SendData{ receiver, data } => {
                        if let Some(p) = peripherals.get(&receiver) {
                            if p.is_connected().await.unwrap_or(false) {
                                let chat_char = p.characteristics().into_iter()
                                    .find(|c| c.uuid == CHARACTERISTIC_UUID);
                                if let Some(c) = chat_char {
                                    if let Err(e) = p.write(&c, &data, WriteType::WithoutResponse).await {
                                        error!("Failed to write to characteristic: {}", e);
                                    }
                                } else {
                                    warn!("Bitchat characteristic not found on peer {:?}. Have services been discovered?", receiver);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
