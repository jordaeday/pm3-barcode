#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::mpsc;
use std::time::Duration;

slint::include_modules!();

const PORT: &str = "/dev/ttyUSB0";
const BAUD: u32 = 115200;

struct Scan {
    kind: &'static str,
    content: String,
    detail: String,
}

fn parse_scan(bytes: &[u8]) -> Scan {
    let raw = String::from_utf8_lossy(bytes);
    let clean: String = raw
        .chars()
        .filter(|&c| c != '\u{FFFD}' && (c >= ' ' || matches!(c, '\t' | '\n' | '\r')))
        .collect();
    let s = clean.trim();

    // WiFi QR code: WIFI:T:WPA;S:ssid;P:pass;;
    if s.to_ascii_uppercase().starts_with("WIFI:") {
        let (mut ssid, mut password, mut security) = (String::new(), String::new(), String::new());
        for part in s[5..].split(';') {
            if let Some(v) = part.strip_prefix("S:").or_else(|| part.strip_prefix("s:")) {
                ssid = v.to_string();
            } else if let Some(v) = part.strip_prefix("P:").or_else(|| part.strip_prefix("p:")) {
                password = v.to_string();
            } else if let Some(v) = part.strip_prefix("T:").or_else(|| part.strip_prefix("t:")) {
                security = v.to_string();
            }
        }
        let detail = format!("Password: {password}  ({security})");
        return Scan { kind: "WiFi", content: ssid, detail };
    }

    // URL
    if s.contains("://") || s.to_ascii_lowercase().starts_with("www.") {
        let host = s
            .splitn(2, "://").nth(1).unwrap_or(s)
            .split(['/', '?', '#']).next().unwrap_or(s)
            .to_string();
        return Scan { kind: "URL", content: host, detail: s.to_string() };
    }

    // mailto: / tel: / geo:
    if let Some(addr) = s.strip_prefix("mailto:").or_else(|| s.strip_prefix("MAILTO:")) {
        return Scan { kind: "Email", content: addr.split('?').next().unwrap_or(addr).to_string(), detail: String::new() };
    }
    if let Some(num) = s.strip_prefix("tel:").or_else(|| s.strip_prefix("TEL:")) {
        return Scan { kind: "Phone", content: num.to_string(), detail: String::new() };
    }
    if let Some(coords) = s.strip_prefix("geo:").or_else(|| s.strip_prefix("GEO:")) {
        return Scan { kind: "Location", content: coords.split(';').next().unwrap_or(coords).to_string(), detail: String::new() };
    }

    // vCard
    let su = s.to_ascii_uppercase();
    if su.contains("BEGIN:VCARD") {
        let name = s.lines()
            .find(|l| l.to_ascii_uppercase().starts_with("FN:"))
            .and_then(|l| l.splitn(2, ':').nth(1))
            .unwrap_or("Unknown Contact")
            .to_string();
        let detail = s.lines()
            .find(|l| { let u = l.to_ascii_uppercase(); u.starts_with("TEL") || u.starts_with("EMAIL") })
            .and_then(|l| l.splitn(2, ':').nth(1))
            .unwrap_or("").to_string();
        return Scan { kind: "Contact", content: name, detail };
    }

    // Numeric barcode — identify format by digit count
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ' ') {
        let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
        let kind = match digits.len() {
            13 => "EAN-13",
            12 => "UPC-A",
            8  => "EAN-8",
            7  => "UPC-E",
            _  => "Barcode",
        };
        return Scan { kind, content: digits, detail: String::new() };
    }

    Scan { kind: "Text", content: s.to_string(), detail: String::new() }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "framebuffer")] {
        use slint_backend_linuxfb::LinuxFbPlatformBuilder;
        let platform = LinuxFbPlatformBuilder::new()
            .with_framebuffer("/dev/fb0")
            .with_input_autodiscovery(true)
            .build()
            .unwrap();
        slint::platform::set_platform(Box::new(platform)).unwrap();
    }

    let ui = AppWindow::new()?;
    let ui_handle = ui.as_weak();

    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(64);

    // Reader: blocking reads, forward chunks over channel
    std::thread::spawn(move || {
        let port = match serial2::SerialPort::open(PORT, BAUD) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to open {PORT}: {e}");
                return;
            }
        };
        let mut buf = [0u8; 256];
        loop {
            match port.read(&mut buf) {
                Ok(n) if n > 0 => { tx.send(buf[..n].to_vec()).ok(); }
                Err(e) if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
                Err(e) => eprintln!("Read error: {e}"),
                _ => {}
            }
        }
    });

    // Accumulator: group bytes separated by 100ms of silence, then classify and display
    std::thread::spawn(move || {
        let mut scan_buf: Vec<u8> = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(bytes) => scan_buf.extend_from_slice(&bytes),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if scan_buf.is_empty() {
                        continue;
                    }
                    let hex = scan_buf
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let scan = parse_scan(&scan_buf);
                    scan_buf.clear();
                    let handle = ui_handle.clone();
                    slint::invoke_from_event_loop(move || {
                        if let Some(ui) = handle.upgrade() {
                            ui.set_scan_type(scan.kind.into());
                            ui.set_scan_content(scan.content.into());
                            ui.set_scan_detail(scan.detail.into());
                            ui.set_scan_hex(hex.into());
                            ui.set_scan_count(ui.get_scan_count() + 1);
                        }
                    })
                    .ok();
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    ui.run()?;
    Ok(())
}
