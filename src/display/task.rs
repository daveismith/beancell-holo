#![allow(clippy::collapsible_if)]
extern crate alloc;

use core::fmt::Write as FmtWrite;
use defmt::{info, warn};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::gpio::{DriveMode, Level, Output, OutputConfig};
use heapless::{String, Vec};

use crate::display::storage::DisplayCredentialStorage;
use crate::display::{
    DISPLAY_CMD_CHANNEL, DISPLAY_STATUS, DisplayCommand, DisplayPlayState, DisplayPowerState,
    DisplayStatus,
};

static mut WS_RX_BUF: [u8; 1024] = [0u8; 1024];
static mut WS_TX_BUF: [u8; 512] = [0u8; 512];

static mut HTTP_RX_BUF: [u8; 1024] = [0u8; 1024];
static mut HTTP_TX_BUF: [u8; 512] = [0u8; 512];

#[embassy_executor::task]
pub async fn display_task(
    relay_pin: esp_hal::gpio::AnyPin<'static>,
    stack: embassy_net::Stack<'static>,
    storage: DisplayCredentialStorage,
) -> ! {
    let mut relay = Output::new(
        relay_pin,
        Level::Low,
        OutputConfig::default().with_drive_mode(DriveMode::PushPull),
    );

    // Initial state: Off
    {
        let mut guard = DISPLAY_STATUS.lock().await;
        *guard = DisplayStatus::default();
    }

    loop {
        let status = { DISPLAY_STATUS.lock().await.clone() };
        if status.power == DisplayPowerState::Off {
            let cmd = DISPLAY_CMD_CHANNEL.receive().await;
            match cmd {
                DisplayCommand::PowerOn => {
                    info!("Display Command: PowerOn received");
                    {
                        let mut guard = DISPLAY_STATUS.lock().await;
                        guard.power = DisplayPowerState::PoweringOn;
                    }
                    relay.set_high();
                    // Brief hardware settled delay
                    Timer::after(Duration::from_millis(500)).await;

                    // Trigger WiFi connection
                    match run_wifi_connect(&storage).await {
                        Ok(()) => {
                            info!("Display AP Connected! Setting defaults...");
                            {
                                let mut guard = DISPLAY_STATUS.lock().await;
                                guard.power = DisplayPowerState::On;
                                guard.wifi_connected = true;
                            }

                            // Initialize TCP Socket for keep-alive HTTP communications
                            let (rx, tx) = unsafe {
                                (
                                    &mut *core::ptr::addr_of_mut!(HTTP_RX_BUF),
                                    &mut *core::ptr::addr_of_mut!(HTTP_TX_BUF),
                                )
                            };
                            let mut http_socket = embassy_net::tcp::TcpSocket::new(stack, rx, tx);
                            http_socket.set_keep_alive(Some(Duration::from_secs(5)));

                            // Initialize boot defaults
                            let mut rx_buf = [0u8; 1536];
                            // Login / establishing session
                            let _ = execute_request(&mut http_socket, "/ctrl/session", &mut rx_buf)
                                .await;

                            // 1. Set volume to 3
                            let _ =
                                execute_request(&mut http_socket, "/ctrl/set?volum=3", &mut rx_buf)
                                    .await;
                            // 2. Set brightness to 3
                            let _ = execute_request(
                                &mut http_socket,
                                "/ctrl/set?brightness=3",
                                &mut rx_buf,
                            )
                            .await;
                            // 3. Set loop mode to "one"
                            let _ = execute_request(
                                &mut http_socket,
                                "/ctrl/set?loop=one",
                                &mut rx_buf,
                            )
                            .await;
                            // 4. Set switcher to off to stop things at startup
                            let _ = execute_request(
                                &mut http_socket,
                                "/ctrl/set?switcher=off",
                                &mut rx_buf,
                            )
                            .await;

                            // 5. DCIM files lookup to cache available files
                            if let Ok((start, end)) =
                                execute_request(&mut http_socket, "/DCIM", &mut rx_buf).await
                            {
                                if let Ok(dcim_body) = core::str::from_utf8(&rx_buf[start..end]) {
                                    let files = parse_dcim_files(dcim_body);
                                    {
                                        let mut guard = DISPLAY_STATUS.lock().await;
                                        guard.files = files;
                                        guard.switcher = Some({
                                            let mut s = String::new();
                                            let _ = s.push_str("off");
                                            s
                                        });
                                    }
                                }
                            }

                            // Now start the active connection monitor
                            run_display_active_loop(&mut http_socket, stack, &storage, &mut relay)
                                .await;
                        }
                        Err(e) => {
                            warn!("Display WiFi connect failed: {:?}", e);
                            relay.set_low();
                            {
                                let mut guard = DISPLAY_STATUS.lock().await;
                                guard.power = DisplayPowerState::Off;
                                guard.wifi_connected = false;
                                guard.play_state = DisplayPlayState::Offline;
                            }
                        }
                    }
                }
                DisplayCommand::SetWifi { ssid, passphrase } => {
                    if storage
                        .save_credentials(ssid.as_str(), passphrase.as_str())
                        .await
                        .is_err()
                    {
                        warn!("Failed to save credential settings");
                    }
                }
                _ => {
                    info!("Display is powered off. Send 'display power on' first.");
                }
            }
        } else {
            Timer::after(Duration::from_millis(100)).await;
        }
    }
}

async fn run_wifi_connect(storage: &DisplayCredentialStorage) -> Result<(), &'static str> {
    if !crate::wifi::wifi_task_ready() {
        return Err("WiFi subsystem not initialized");
    }

    let saved = storage.load_credentials().await.ok().flatten();
    let mut target_ssid = saved.as_ref().map(|(s, _)| s.clone());
    let mut target_pass = saved.as_ref().map(|(_, p)| p.clone());

    info!("Starting dynamic scan for display AP...");
    let mut found = false;

    // Retry scanning for up to 12 attempts (roughly 15-20 seconds maximum timeout)
    for attempt in 1..=12 {
        info!("Scanning for display AP (attempt {}/12)...", attempt);
        crate::wifi::WIFI_CMD_CHANNEL
            .send(crate::wifi::WifiCommand::Scan)
            .await;

        // Wait for scan to complete: poll status (up to 40 times 100ms = 4 seconds per scan)
        let mut scan_ok = false;
        for _ in 0..40 {
            Timer::after_millis(100).await;
            let status = crate::wifi::WIFI_STATUS.lock().await.clone();
            if status.state != crate::wifi::WifiState::Scanning {
                scan_ok = true;
                break;
            }
        }

        if !scan_ok {
            warn!("Scan attempt timed out, retrying");
            continue;
        }

        let scan = crate::wifi::WIFI_SCAN_RESULTS.lock().await.clone();
        if let Some(ref target) = target_ssid {
            // Case A: we have a saved SSID, check if it's visible
            for entry in &scan.entries {
                if entry.ssid == *target {
                    info!("Target AP {} is alive and visible!", target.as_str());
                    found = true;
                    break;
                }
            }
        } else {
            // Case B: no saved credential, scan for any match starting with "5D_"
            for entry in &scan.entries {
                if entry.ssid.starts_with("5D_") {
                    info!("Found matching display AP: {}", entry.ssid.as_str());
                    let default_pass = "12345678";
                    let mut p = heapless::String::new();
                    let _ = p.push_str(default_pass);

                    if storage
                        .save_credentials(entry.ssid.as_str(), default_pass)
                        .await
                        .is_err()
                    {
                        warn!("Failed to save automatically discovered credentials");
                    }

                    target_ssid = Some(entry.ssid.clone());
                    target_pass = Some(p);
                    found = true;
                    break;
                }
            }
        }

        if found {
            break;
        }

        // If not found yet, sleep 500ms before checking again
        Timer::after_millis(500).await;
    }

    let (ssid, passphrase) = match (target_ssid, target_pass) {
        (Some(s), Some(p)) if found => (s, p),
        _ => {
            return Err(
                "Display AP not found (either saved SSID was not visible or no matching 5D_ network was found)",
            );
        }
    };

    info!("Connecting to display WiFi AP: {}...", ssid.as_str());
    crate::wifi::WIFI_CMD_CHANNEL
        .send(crate::wifi::WifiCommand::Connect {
            ssid: ssid.clone(),
            passphrase,
        })
        .await;

    // Wait up to 30 seconds for the connect to succeed
    for _ in 0..60 {
        Timer::after_millis(500).await;
        let status = crate::wifi::WIFI_STATUS.lock().await.clone();
        if status.state == crate::wifi::WifiState::Connected {
            if let Some(current_ssid) = status.ssid {
                if current_ssid == ssid {
                    info!("Successfully connected to display AP.");
                    return Ok(());
                }
            }
        }
    }

    Err("WiFi connection timeout/failed")
}

async fn run_display_active_loop(
    http_socket: &mut embassy_net::tcp::TcpSocket<'_>,
    stack: embassy_net::Stack<'static>,
    _storage: &DisplayCredentialStorage,
    relay: &mut Output<'static>,
) {
    let command_loop = async {
        let mut rx_buf = [0u8; 1024];
        loop {
            let cmd = DISPLAY_CMD_CHANNEL.receive().await;
            if handle_command_active(http_socket, cmd, relay, &mut rx_buf).await {
                break;
            }
        }
    };

    let ws_loop = async {
        let mut last_ws_attempt: Option<Instant> = None;
        let mut heartbeat_msg_id = 1;

        loop {
            let now = Instant::now();
            if let Some(last) = last_ws_attempt {
                let elapsed = now.duration_since(last);
                if elapsed < Duration::from_secs(5) {
                    Timer::after(Duration::from_secs(5) - elapsed).await;
                }
            }

            last_ws_attempt = Some(Instant::now());
            info!("Attempting to establish WebSocket monitor connection on port 9000...");
            let (rx, tx) = unsafe {
                (
                    &mut *core::ptr::addr_of_mut!(WS_RX_BUF),
                    &mut *core::ptr::addr_of_mut!(WS_TX_BUF),
                )
            };
            let mut socket = embassy_net::tcp::TcpSocket::new(stack, rx, tx);
            socket.set_keep_alive(Some(Duration::from_secs(5)));
            let remote = embassy_net::IpEndpoint::new(
                embassy_net::IpAddress::Ipv4(embassy_net::Ipv4Address::new(192, 168, 4, 1)),
                9000,
            );

            if let Ok(Ok(())) = with_timeout(Duration::from_secs(2), socket.connect(remote)).await {
                if ws_handshake(&mut socket).await.is_ok() {
                    info!("WebSocket monitor established success!");
                    {
                        let mut guard = DISPLAY_STATUS.lock().await;
                        guard.wifi_connected = true;
                    }
                    let mut last_heartbeat = Instant::now();

                    if run_ws_read_heartbeat_loop(
                        &mut socket,
                        &mut last_heartbeat,
                        &mut heartbeat_msg_id,
                    )
                    .await
                    .is_err()
                    {
                        warn!("WebSocket read/heartbeat loop error");
                    }

                    {
                        let mut guard = DISPLAY_STATUS.lock().await;
                        guard.wifi_connected = false;
                        guard.play_state = DisplayPlayState::Offline;
                    }
                } else {
                    warn!("WebSocket handshake failed");
                }
            } else {
                warn!("WebSocket tcp connect failed");
            }
        }
    };

    select(command_loop, ws_loop).await;
}

async fn run_ws_read_heartbeat_loop(
    socket: &mut embassy_net::tcp::TcpSocket<'_>,
    last_heartbeat: &mut Instant,
    heartbeat_msg_id: &mut u32,
) -> Result<(), ()> {
    let mut rx_buf = [0u8; 1024];
    let mut rx_len = 0;

    loop {
        let now = Instant::now();
        let hb_elapsed = now.duration_since(*last_heartbeat);
        let next_hb_check = if hb_elapsed >= Duration::from_secs(15) {
            *last_heartbeat = now;
            let mut hb_payload = heapless::String::<64>::new();
            if write!(
                &mut hb_payload,
                "{{\"cmd\": \"heartbeat\", \"msgId\": \"{}\"}}",
                *heartbeat_msg_id
            )
            .is_ok()
            {
                *heartbeat_msg_id += 1;
                let mut frame_buf = [0u8; 128];
                let frame_len = ws_frame_text(hb_payload.as_str(), &mut frame_buf);
                use embedded_io_async::Write;
                info!("Sending WS Heartbeat: {}", hb_payload.as_str());
                if socket.write_all(&frame_buf[..frame_len]).await.is_err() {
                    return Err(());
                }
            }
            Duration::from_secs(15)
        } else {
            Duration::from_secs(15) - hb_elapsed
        };

        if rx_len >= rx_buf.len() {
            warn!("WS rx buffer is full, resetting connection");
            return Err(());
        }

        match select(
            socket.read(&mut rx_buf[rx_len..]),
            Timer::after(next_hb_check),
        )
        .await
        {
            Either::First(Ok(0)) => {
                warn!("WS socket closed by remote");
                return Err(());
            }
            Either::First(Ok(n)) => {
                rx_len += n;

                while rx_len > 0 {
                    if let Some((header_len, payload_len)) =
                        parse_incoming_ws_frame_buffered(&rx_buf[..rx_len])
                    {
                        let total_len = header_len + payload_len;
                        let opcode = rx_buf[0] & 0x0F;
                        let masked = (rx_buf[1] & 0x80) != 0;

                        if opcode == 1 {
                            let mut success = false;
                            if masked {
                                let mut temp_payload = [0u8; 512];
                                let mut actual_payload_len = payload_len;
                                if actual_payload_len > temp_payload.len() {
                                    actual_payload_len = temp_payload.len();
                                }
                                let mask_offset = header_len.saturating_sub(4);
                                let mask = &rx_buf[mask_offset..mask_offset + 4];
                                for i in 0..actual_payload_len {
                                    temp_payload[i] = rx_buf[header_len + i] ^ mask[i % 4];
                                }
                                if let Ok(frame) =
                                    core::str::from_utf8(&temp_payload[..actual_payload_len])
                                {
                                    //info!("Incoming WS Frame payload: {}", frame);
                                    process_onplay_frame(frame).await;
                                    success = true;
                                }
                            } else {
                                if let Ok(frame) =
                                    core::str::from_utf8(&rx_buf[header_len..total_len])
                                {
                                    //info!("Incoming WS Frame payload: {}", frame);
                                    process_onplay_frame(frame).await;
                                    success = true;
                                }
                            }
                            if !success {
                                warn!(
                                    "WS: Failed to parse UTF-8 text frame of payload length={}",
                                    payload_len
                                );
                            }
                        } else if opcode == 9 {
                            info!("WS: Received Ping frame. Sending Pong.");
                            let pong = [0x8A, 0x00];
                            use embedded_io_async::Write;
                            if socket.write_all(&pong).await.is_err() {
                                return Err(());
                            }
                        } else {
                            info!(
                                "WS: Received non-text frame with opcode={}, len={}",
                                opcode, payload_len
                            );
                        }

                        // Shift consumed bytes
                        rx_buf.copy_within(total_len..rx_len, 0);
                        rx_len -= total_len;
                    } else {
                        // Incomplete frame, wait for more data
                        break;
                    }
                }
            }
            Either::First(Err(_)) => {
                warn!("WS socket read error");
                return Err(());
            }
            Either::Second(_) => {
                // Heartbeat check interval elapsed, continue loop to trigger heartbeat send
            }
        }
    }
}

async fn process_onplay_frame(frame: &str) {
    if let Some((current, progress, playing, total)) = extract_onplayinfo(frame) {
        /*info!(
            "Parsed OnPlayInfo: file={}, progress={}, playing={}, total={}",
            current.as_str(),
            progress,
            playing,
            total
        );*/
        let mut guard = DISPLAY_STATUS.lock().await;
        guard.current_file = Some(current);
        guard.progress = progress;
        guard.total = total;
        guard.play_state = if playing {
            DisplayPlayState::Playing
        } else {
            DisplayPlayState::Paused
        };
    }
}

async fn handle_command_active(
    socket: &mut embassy_net::tcp::TcpSocket<'_>,
    cmd: DisplayCommand,
    relay: &mut Output<'static>,
    rx_buf: &mut [u8],
) -> bool {
    match cmd {
        DisplayCommand::PowerOff => {
            info!("Powering down display...");
            {
                let mut guard = DISPLAY_STATUS.lock().await;
                guard.power = DisplayPowerState::PoweringOff;
            }
            let _ = execute_request(socket, "/ctrl/session?action=quit", rx_buf).await;
            relay.set_low();

            let _ = crate::wifi::WIFI_CMD_CHANNEL
                .send(crate::wifi::WifiCommand::Disconnect)
                .await;

            {
                let mut guard = DISPLAY_STATUS.lock().await;
                *guard = DisplayStatus::default();
            }
            info!("Power off complete.");
            true
        }
        DisplayCommand::PowerOn => {
            info!("Display already powered on.");
            false
        }
        DisplayCommand::PlayFile { filename_hex } => {
            // First turn on switcher so play command is actually processed by device
            let _ = execute_request(socket, "/ctrl/set?switcher=on", rx_buf).await;
            {
                let mut guard = DISPLAY_STATUS.lock().await;
                guard.switcher = Some({
                    let mut s = String::new();
                    let _ = s.push_str("on");
                    s
                });
            }

            let mut req = heapless::String::<128>::new();
            if write!(&mut req, "/DCIM/{}?act=play", filename_hex.as_str()).is_ok() {
                let _ = execute_request(socket, req.as_str(), rx_buf).await;
            }
            false
        }
        DisplayCommand::Pause => {
            let _ = execute_request(socket, "/ctrl/play?action=pause", rx_buf).await;
            false
        }
        DisplayCommand::Next => {
            let _ = execute_request(socket, "/ctrl/play?action=next", rx_buf).await;
            false
        }
        DisplayCommand::Previous => {
            let _ = execute_request(socket, "/ctrl/play?action=previous", rx_buf).await;
            false
        }
        DisplayCommand::SetBrightness(b) => {
            let mut req = heapless::String::<64>::new();
            if write!(&mut req, "/ctrl/set?brightness={}", b).is_ok() {
                let _ = execute_request(socket, req.as_str(), rx_buf).await;
            }
            false
        }
        DisplayCommand::SetVolume(v) => {
            let mut req = heapless::String::<64>::new();
            if write!(&mut req, "/ctrl/set?volum={}", v).is_ok() {
                let _ = execute_request(socket, req.as_str(), rx_buf).await;
            }
            false
        }
        DisplayCommand::SetLoop(mode) => {
            let mut req = heapless::String::<64>::new();
            if write!(&mut req, "/ctrl/set?loop={}", mode.as_str()).is_ok() {
                let _ = execute_request(socket, req.as_str(), rx_buf).await;
            }
            false
        }
        DisplayCommand::SetWifi { .. } => false,
        DisplayCommand::GetDcim => {
            if let Ok((start, end)) = execute_request(socket, "/DCIM", rx_buf).await {
                if let Ok(body) = core::str::from_utf8(&rx_buf[start..end]) {
                    let files = parse_dcim_files(body);
                    let mut guard = DISPLAY_STATUS.lock().await;
                    guard.files = files;
                }
            }
            false
        }
        DisplayCommand::GetConfig => {
            if let Ok((start, end)) = execute_request(socket, "/ctrl/get?k=all", rx_buf).await {
                if let Ok(body) = core::str::from_utf8(&rx_buf[start..end]) {
                    let brightness = find_json_int_value(body, "\"brightness\"").map(|v| v as u8);
                    let volume = find_json_int_value(body, "\"volum\"").map(|v| v as u8);
                    let loop_mode = find_json_str_value::<10>(body, "\"loop\"");
                    let angle = find_json_int_value(body, "\"angle\"").map(|v| v as u16);
                    let ble = find_json_str_value::<10>(body, "\"ble\"");
                    let switcher = find_json_str_value::<10>(body, "\"switch\"");
                    let ssid_config = find_json_str_value::<32>(body, "\"ssid\"");

                    let mut guard = DISPLAY_STATUS.lock().await;
                    if let Some(b) = brightness {
                        guard.brightness = b;
                    }
                    if let Some(v) = volume {
                        guard.volume = v;
                    }
                    if let Some(l) = loop_mode {
                        guard.loop_mode = Some(l);
                    }
                    guard.angle = angle;
                    guard.ble = ble;
                    guard.switcher = switcher;
                    guard.ssid_config = ssid_config;

                    // Parse dynamic configs key-value pairs
                    parse_config_response(body, &mut guard.configs);
                }
            }
            false
        }
        DisplayCommand::SetConfig { key, value } => {
            let mut req = heapless::String::<128>::new();
            if write!(&mut req, "/ctrl/set?{}={}", key.as_str(), value.as_str()).is_ok() {
                let _ = execute_request(socket, req.as_str(), rx_buf).await;
            }
            false
        }
        DisplayCommand::GetStatus => {
            if let Ok((start, end)) =
                execute_request(socket, "/ctrl/play?action=status", rx_buf).await
            {
                if let Ok(body) = core::str::from_utf8(&rx_buf[start..end]) {
                    let current = if let Some(c_idx) = body.find("\"current\":") {
                        let sub = &body[c_idx + 10..];
                        if let Some(q1) = sub.find('"') {
                            if let Some(q2) = sub[q1 + 1..].find('"') {
                                let hex_name = &sub[q1 + 1..q1 + 1 + q2];
                                let mut h = heapless::String::new();
                                let _ = h.push_str(hex_name);
                                Some(h)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let progress = find_json_int_value(body, "\"progress\"");
                    let total = find_json_int_value(body, "\"total\"");
                    let playing = if let Some(s_idx) = body.find("\"state\":") {
                        let sub = &body[s_idx + 8..];
                        if let Some(q1) = sub.find('"') {
                            if let Some(q2) = sub[q1 + 1..].find('"') {
                                let state_val = &sub[q1 + 1..q1 + 1 + q2];
                                state_val == "ing" || state_val == "playing"
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    let mut guard = DISPLAY_STATUS.lock().await;
                    if let Some(c) = current {
                        guard.current_file = Some(c);
                    }
                    if let Some(p) = progress {
                        guard.progress = p;
                    }
                    if let Some(t) = total {
                        guard.total = t;
                    }
                    guard.play_state = if playing {
                        DisplayPlayState::Playing
                    } else {
                        DisplayPlayState::Paused
                    };
                }
            }
            false
        }
        DisplayCommand::SetSwitcher(on) => {
            let mut req = heapless::String::<64>::new();
            let val_str = if on { "on" } else { "off" };
            if write!(&mut req, "/ctrl/set?switcher={}", val_str).is_ok() {
                let _ = execute_request(socket, req.as_str(), rx_buf).await;
                let mut guard = DISPLAY_STATUS.lock().await;
                guard.switcher = Some({
                    let mut s = String::new();
                    let _ = s.push_str(val_str);
                    s
                });
            }
            false
        }
        DisplayCommand::GetInfo => {
            if let Ok((start, end)) = execute_request(socket, "/info", rx_buf).await {
                if let Ok(body) = core::str::from_utf8(&rx_buf[start..end]) {
                    let model = find_json_flat_str_value::<16>(body, "\"model\"");
                    let sw_version = find_json_flat_str_value::<16>(body, "\"sw\"");
                    let mut guard = DISPLAY_STATUS.lock().await;
                    guard.model = model;
                    guard.sw_version = sw_version;
                }
            }
            false
        }
    }
}

fn find_content_length(headers: &str) -> Option<usize> {
    let target = "Content-Length:";
    if let Some(idx) = headers.find("Content-Length:") {
        let sub = &headers[idx + target.len()..];
        let end_idx = sub.find("\r\n")?;
        let val_str = sub[..end_idx].trim();
        val_str.parse::<usize>().ok()
    } else if let Some(idx) = headers.find("content-length:") {
        let target_lower = "content-length:";
        let sub = &headers[idx + target_lower.len()..];
        let end_idx = sub.find("\r\n")?;
        let val_str = sub[..end_idx].trim();
        val_str.parse::<usize>().ok()
    } else {
        None
    }
}

async fn read_http_response(
    socket: &mut embassy_net::tcp::TcpSocket<'_>,
    rx_buf: &mut [u8],
) -> Result<usize, ()> {
    let mut total_read = 0;
    let mut headers_parsed = false;
    let mut body_start_offset = 0;
    let mut expected_body_len = None;

    loop {
        let read_fut = socket.read(&mut rx_buf[total_read..]);
        if headers_parsed {
            if let Some(expected_len) = expected_body_len {
                let current_body_len = total_read - body_start_offset;
                if current_body_len >= expected_len {
                    info!(
                        "HTTP: Fully received expected body of {} bytes",
                        expected_len
                    );
                    break;
                }
            }
        }

        let timeout_dur = if headers_parsed {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(1500)
        };

        match with_timeout(timeout_dur, read_fut).await {
            Ok(Ok(0)) => {
                info!("HTTP: Connection closed by remote");
                break;
            }
            Ok(Ok(n)) => {
                total_read += n;
                if total_read >= rx_buf.len() {
                    warn!("HTTP: Rx buffer full");
                    break;
                }

                if !headers_parsed {
                    if let Ok(temp_str) = core::str::from_utf8(&rx_buf[..total_read]) {
                        if let Some(delim_idx) = temp_str.find("\r\n\r\n") {
                            headers_parsed = true;
                            body_start_offset = delim_idx + 4;
                            let headers = &temp_str[..delim_idx];
                            expected_body_len = find_content_length(headers);
                            if let Some(len) = expected_body_len {
                                info!("HTTP: Parsed Content-Length: {}", len);
                            }
                        }
                    }
                }
            }
            Ok(Err(_)) => {
                warn!("HTTP: Read error");
                return Err(());
            }
            Err(_) => {
                if headers_parsed {
                    info!(
                        "HTTP: Read timeout on active keep-alive connection, assuming response complete (read {} bytes)",
                        total_read
                    );
                    break;
                } else {
                    warn!("HTTP: Timeout waiting for headers");
                    return Err(());
                }
            }
        }
    }
    Ok(total_read)
}

async fn execute_request(
    socket: &mut embassy_net::tcp::TcpSocket<'_>,
    path: &str,
    rx_buf: &mut [u8],
) -> Result<(usize, usize), ()> {
    let (start, end) = http_get(socket, path, rx_buf).await?;
    if let Ok(body) = core::str::from_utf8(&rx_buf[start..end]) {
        if body.contains("No active session") || body.contains("\"code\": 1") {
            info!("No active session detected, initiating login /ctrl/session...");
            let mut login_buf = [0u8; 256];
            let _ = http_get(socket, "/ctrl/session", &mut login_buf).await?;
            return http_get(socket, path, rx_buf).await;
        }
    }
    Ok((start, end))
}

async fn http_get(
    socket: &mut embassy_net::tcp::TcpSocket<'_>,
    path: &str,
    rx_buf: &mut [u8],
) -> Result<(usize, usize), ()> {
    match with_timeout(
        Duration::from_millis(1500),
        http_get_inner(socket, path, rx_buf),
    )
    .await
    {
        Ok(Ok(res)) => Ok(res),
        _ => {
            warn!("HTTP: GET failed on existing socket, reconnecting and retrying...");
            socket.abort();
            with_timeout(
                Duration::from_millis(1500),
                http_get_inner(socket, path, rx_buf),
            )
            .await
            .map_err(|_| ())?
        }
    }
}

async fn http_get_inner(
    socket: &mut embassy_net::tcp::TcpSocket<'_>,
    path: &str,
    rx_buf: &mut [u8],
) -> Result<(usize, usize), ()> {
    use embassy_net::tcp::State;

    let remote = embassy_net::IpEndpoint::new(
        embassy_net::IpAddress::Ipv4(embassy_net::Ipv4Address::new(192, 168, 4, 1)),
        80,
    );

    if socket.state() != State::Established {
        if socket.state() != State::Closed {
            socket.abort();
        }
        info!("HTTP: Connecting to 192.168.4.1:80 for path: {}", path);
        match with_timeout(Duration::from_secs(3), socket.connect(remote)).await {
            Ok(Ok(())) => {
                info!("HTTP: Connected successfully to Port 80 for path: {}", path);
            }
            Ok(Err(e)) => {
                warn!(
                    "HTTP: Connection immediately failed for path {}: {:?}",
                    path,
                    defmt::Debug2Format(&e)
                );
                return Err(());
            }
            Err(_) => {
                warn!("HTTP: Connection timed out for path: {}", path);
                return Err(());
            }
        }
    }

    use embedded_io_async::Write;
    let mut req = heapless::String::<160>::new();
    if write!(
        &mut req,
        "GET {} HTTP/1.1\r\nHost: 192.168.4.1\r\nConnection: keep-alive\r\n\r\n",
        path
    )
    .is_err()
    {
        return Err(());
    }

    info!("HTTP: Sending GET {}", path);
    if socket.write_all(req.as_bytes()).await.is_err() {
        warn!("HTTP: Send failed");
        return Err(());
    }

    let total_read = read_http_response(socket, rx_buf).await?;

    let response_str = core::str::from_utf8(&rx_buf[..total_read]).map_err(|_| ())?;
    info!(
        "HTTP raw response for {}: (total_read={}) \n--- RAW RESPONSE START ---\n{}\n--- RAW RESPONSE END ---",
        path, total_read, response_str
    );

    if let Some(body_start) = response_str.find("\r\n\r\n") {
        Ok((body_start + 4, total_read))
    } else {
        Ok((0, total_read))
    }
}

async fn ws_handshake(socket: &mut embassy_net::tcp::TcpSocket<'_>) -> Result<(), ()> {
    let req = "GET / HTTP/1.1\r\nHost: 192.168.4.1:9000\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    use embedded_io_async::Write;
    info!("WS: Sending WebSocket Handshake...");
    socket.write_all(req.as_bytes()).await.map_err(|_| ())?;

    let mut rx_buf = [0u8; 512];
    let n = socket.read(&mut rx_buf).await.map_err(|_| ())?;
    let resp = core::str::from_utf8(&rx_buf[..n]).map_err(|_| ())?;
    info!("WS: Handshake response: {}", resp);
    if resp.contains("101") || resp.contains("Upgrade") {
        info!("WS: Handshake Succeeded.");
        Ok(())
    } else {
        warn!("WS: Handshake Failed.");
        Err(())
    }
}

fn ws_frame_text(payload: &str, out: &mut [u8]) -> usize {
    let len = payload.len();
    out[0] = 0x81;
    out[1] = 0x80 | (len as u8);
    out[2..6].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);

    for i in 0..len {
        let mask = [0x12, 0x34, 0x56, 0x78];
        out[6 + i] = payload.as_bytes()[i] ^ mask[i % 4];
    }
    6 + len
}

fn parse_incoming_ws_frame_buffered(buf: &[u8]) -> Option<(usize, usize)> {
    if buf.len() < 2 {
        return None;
    }
    let masked = (buf[1] & 0x80) != 0;
    let mut payload_len = (buf[1] & 0x7F) as usize;

    let mut header_len = 2;
    if payload_len == 126 {
        header_len += 2;
    } else if payload_len == 127 {
        header_len += 8;
    }
    if masked {
        header_len += 4;
    }

    if buf.len() < header_len {
        return None;
    }

    if payload_len == 126 {
        payload_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    } else if payload_len == 127 {
        payload_len = u64::from_be_bytes([
            buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8], buf[9],
        ]) as usize;
    }

    let total_len = header_len + payload_len;
    if buf.len() < total_len {
        return None;
    }

    Some((header_len, payload_len))
}

fn parse_dcim_files(body: &str) -> Vec<String<64>, 16> {
    let mut files = Vec::new();
    if let Some(array_start) = body.find('[') {
        if let Some(array_end) = body[array_start..].find(']') {
            let array_content = &body[array_start + 1..array_start + array_end];
            let mut file_search = array_content;
            while let Some(q1) = file_search.find('"') {
                let remaining = &file_search[q1 + 1..];
                if let Some(q2) = remaining.find('"') {
                    let file_hex = &remaining[..q2];
                    if !file_hex.is_empty() {
                        let mut h_str = String::new();
                        if h_str.push_str(file_hex).is_ok() {
                            let _ = files.push(h_str);
                        }
                    }
                    file_search = &remaining[q2 + 1..];
                } else {
                    break;
                }
            }
        }
    }
    files
}

fn find_json_int_value(body: &str, key: &str) -> Option<u32> {
    if let Some(key_idx) = body.find(key) {
        let sub = &body[key_idx..];
        if let Some(val_idx) = sub.find("\"value\":") {
            let val_sub = &sub[val_idx + 8..];
            let mut num_str = "";
            for (i, c) in val_sub.char_indices() {
                if c.is_ascii_whitespace() {
                    continue;
                }
                if c.is_ascii_digit() {
                    // keep scanning digits
                } else if i > 0 {
                    num_str = &val_sub[..i];
                    break;
                } else {
                    break;
                }
            }
            if !num_str.is_empty() {
                if let Ok(num) = num_str.trim().parse::<u32>() {
                    return Some(num);
                }
            }
        } else {
            // Check direct colon parsing (for WS status payload format like `progress:47`)
            if let Some(col_idx) = sub.find(':') {
                let val_sub = &sub[col_idx + 1..];
                let mut num_str = "";
                for (i, c) in val_sub.char_indices() {
                    if c.is_ascii_whitespace() {
                        continue;
                    }
                    if c.is_ascii_digit() {
                        // keep scanning
                    } else if i > 0 {
                        num_str = &val_sub[..i];
                        break;
                    } else {
                        break;
                    }
                }
                if !num_str.is_empty() {
                    if let Ok(num) = num_str.trim().parse::<u32>() {
                        return Some(num);
                    }
                }
            }
        }
    }
    None
}

fn find_json_str_value<const N: usize>(body: &str, key: &str) -> Option<String<N>> {
    if let Some(key_idx) = body.find(key) {
        let sub = &body[key_idx..];
        if let Some(val_idx) = sub.find("\"value\":") {
            let val_sub = &sub[val_idx + 8..];
            if let Some(q1) = val_sub.find('"') {
                if let Some(q2) = val_sub[q1 + 1..].find('"') {
                    let s = &val_sub[q1 + 1..q1 + 1 + q2];
                    let mut h_str = String::new();
                    if h_str.push_str(s).is_ok() {
                        return Some(h_str);
                    }
                }
            }
        }
    }
    None
}

fn find_json_flat_str_value<const N: usize>(body: &str, key: &str) -> Option<String<N>> {
    if let Some(key_idx) = body.find(key) {
        let sub = &body[key_idx..];
        if let Some(col_idx) = sub.find(':') {
            let val_sub = &sub[col_idx + 1..];
            if let Some(q1) = val_sub.find('"') {
                if let Some(q2) = val_sub[q1 + 1..].find('"') {
                    let s = &val_sub[q1 + 1..q1 + 1 + q2];
                    let mut h_str = String::new();
                    if h_str.push_str(s).is_ok() {
                        return Some(h_str);
                    }
                }
            }
        }
    }
    None
}

fn parse_config_response(body: &str, configs: &mut Vec<crate::display::ConfigEntry, 10>) {
    configs.clear();
    let mut search_ptr = body;
    while let Some(key_idx) = search_ptr.find("\"key\":") {
        let key_sub = &search_ptr[key_idx + 6..];
        let Some(q1) = key_sub.find('"') else {
            break;
        };
        let Some(q2) = key_sub[q1 + 1..].find('"') else {
            break;
        };
        let key_str = &key_sub[q1 + 1..q1 + 1 + q2];

        let Some(val_idx) = key_sub.find("\"value\":") else {
            break;
        };
        let val_sub = key_sub[val_idx + 8..].trim_start();

        let mut val_string = String::<64>::new();

        if let Some(stripped) = val_sub.strip_prefix('"') {
            let mut escaped = false;
            for c in stripped.chars() {
                if escaped {
                    let _ = val_string.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                    let _ = val_string.push(c);
                } else if c == '"' {
                    break;
                } else {
                    let _ = val_string.push(c);
                }
            }
        } else {
            for c in val_sub.chars() {
                if c == ',' || c == '}' || c == '\r' || c == '\n' {
                    break;
                }
                if val_string.push(c).is_err() {
                    break;
                }
            }
        }

        let mut k = String::<16>::new();
        let _ = k.push_str(key_str.trim());

        let trimmed_val_str = val_string.as_str().trim();
        let mut v = String::<64>::new();
        let _ = v.push_str(trimmed_val_str);

        if !k.is_empty() {
            let _ = configs.push(crate::display::ConfigEntry { key: k, value: v });
        }

        search_ptr = &key_sub[q1 + 1 + q2..];
    }
}

fn extract_onplayinfo(body: &str) -> Option<(String<64>, u32, bool, u32)> {
    let current = if let Some(c_idx) = body.find("\"current\":") {
        let sub = &body[c_idx + 10..];
        if let Some(q1) = sub.find('"') {
            if let Some(q2) = sub[q1 + 1..].find('"') {
                let hex_name = &sub[q1 + 1..q1 + 1 + q2];
                let mut h = String::new();
                let _ = h.push_str(hex_name);
                Some(h)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let progress = find_json_int_value(body, "\"progress\"");
    let total = find_json_int_value(body, "\"total\"");

    let is_playing = if let Some(s_idx) = body.find("\"state\":") {
        let sub = &body[s_idx + 8..];
        if let Some(q1) = sub.find('"') {
            if let Some(q2) = sub[q1 + 1..].find('"') {
                let state_val = &sub[q1 + 1..q1 + 1 + q2];
                state_val == "ing" || state_val == "playing"
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    match (current, progress, total) {
        (Some(c), Some(p), Some(t)) => Some((c, p, is_playing, t)),
        _ => None,
    }
}
