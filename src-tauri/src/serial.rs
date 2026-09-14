//! Serial console sessions: enumerate ports, open them with the usual
//! UART settings and stream bytes between the port and the frontend as
//! `serial-data-<id>` / `serial-exit-<id>` events.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};
use tauri::{AppHandle, Emitter};

static COUNTER: AtomicU64 = AtomicU64::new(1);

/// One serial device as shown in the port picker.
#[derive(Serialize, Clone)]
pub struct PortInfo {
    pub name: String,
    /// USB product/manufacturer or bus type, when the OS reports it.
    pub description: String,
}

/// UART line settings from the serial modal.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialCfg {
    pub baud: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    /// "none" | "odd" | "even"
    pub parity: String,
    /// "none" | "hardware" | "software"
    pub flow: String,
}

/// Enumerate serial devices for the picker. Returns an empty list when
/// enumeration itself fails (e.g. no serial support in the OS image).
pub fn list_ports() -> Vec<PortInfo> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let description = match p.port_type {
                serialport::SerialPortType::UsbPort(u) => {
                    u.product.or(u.manufacturer).unwrap_or_else(|| {
                        format!("USB {:04x}:{:04x}", u.vid, u.pid)
                    })
                }
                serialport::SerialPortType::PciPort => "PCI".into(),
                serialport::SerialPortType::BluetoothPort => "Bluetooth".into(),
                serialport::SerialPortType::Unknown => String::new(),
            };
            PortInfo {
                name: p.port_name,
                description,
            }
        })
        .collect()
}

enum Cmd {
    Data(Vec<u8>),
    /// RS-232 break condition — the classic console attention signal.
    Break,
    Close,
}

struct Entry {
    tx: Sender<Cmd>,
}

#[derive(Clone)]
pub struct SerialManager {
    sessions: Arc<Mutex<HashMap<String, Entry>>>,
}

impl SerialManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Open `port_name` with `cfg` and spawn the I/O worker. Returns the
    /// session id used in event names and write/close calls.
    pub fn connect(
        &self,
        app: &AppHandle,
        port_name: &str,
        cfg: &SerialCfg,
    ) -> Result<String, String> {
        let port = serialport::new(port_name, cfg.baud)
            .data_bits(match cfg.data_bits {
                5 => DataBits::Five,
                6 => DataBits::Six,
                7 => DataBits::Seven,
                _ => DataBits::Eight,
            })
            .stop_bits(if cfg.stop_bits == 2 {
                StopBits::Two
            } else {
                StopBits::One
            })
            .parity(match cfg.parity.as_str() {
                "odd" => Parity::Odd,
                "even" => Parity::Even,
                _ => Parity::None,
            })
            .flow_control(match cfg.flow.as_str() {
                "hardware" => FlowControl::Hardware,
                "software" => FlowControl::Software,
                _ => FlowControl::None,
            })
            // Short timeout makes read() poll so the loop stays responsive
            // to write/close commands (~60ms worst-case write latency).
            .timeout(Duration::from_millis(60))
            .open()
            .map_err(|e| format!("cannot open {port_name}: {e}"))?;

        let id = format!("serial-{}", COUNTER.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel::<Cmd>();
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), Entry { tx });
        thread::spawn({
            let app = app.clone();
            let sid = id.clone();
            move || worker(app, sid, port, rx)
        });
        Ok(id)
    }

    fn send(&self, id: &str, cmd: Cmd) -> Result<(), String> {
        let sessions = self.sessions.lock().unwrap();
        match sessions.get(id) {
            Some(e) => e.tx.send(cmd).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    pub fn write(&self, id: &str, data: Vec<u8>) -> Result<(), String> {
        self.send(id, Cmd::Data(data))
    }

    pub fn send_break(&self, id: &str) -> Result<(), String> {
        self.send(id, Cmd::Break)
    }

    pub fn close(&self, id: &str) {
        let e = self.sessions.lock().unwrap().remove(id);
        if let Some(e) = e {
            let _ = e.tx.send(Cmd::Close);
        }
    }

    /// Kill every open port (webview reload leaves them orphaned).
    pub fn disconnect_all(&self) {
        let ids: Vec<String> = self.sessions.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.close(&id);
        }
    }
}

/// One thread per port: drain pending commands, then poll-read. The 60ms
/// port timeout bounds write latency without busy-looping.
fn worker(app: AppHandle, sid: String, mut port: Box<dyn SerialPort>, rx: Receiver<Cmd>) {
    let data_ev = format!("serial-data-{sid}");
    let exit_ev = format!("serial-exit-{sid}");
    let mut buf = [0u8; 8192];
    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Data(d)) => {
                    if port.write_all(&d).is_err() {
                        let _ = app.emit(&exit_ev, "write failed".to_string());
                        return;
                    }
                }
                Ok(Cmd::Break) => {
                    // serialport 4.x exposes set/clear rather than a timed
                    // send_break — hold the break for ~300ms.
                    let _ = port.set_break();
                    thread::sleep(Duration::from_millis(300));
                    let _ = port.clear_break();
                }
                Ok(Cmd::Close) | Err(mpsc::TryRecvError::Disconnected) => {
                    let _ = app.emit(&exit_ev, "closed".to_string());
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        match port.read(&mut buf) {
            Ok(n) if n > 0 => {
                let _ = app.emit(&data_ev, buf[..n].to_vec());
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => {
                let _ = app.emit(&exit_ev, format!("read error: {e}"));
                return;
            }
        }
    }
}
