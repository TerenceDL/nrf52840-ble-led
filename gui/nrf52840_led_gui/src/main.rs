use anyhow::{anyhow, Context, Result};
use btleplug::api::{
    Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;

// LED characteristic UUID (from firmware)
const LED_CHAR_UUID: &str = "9e7312e0-2354-11eb-9f10-fbc30a63cf38";

#[derive(Debug, Clone)]
struct DeviceInfo {
    addr: String,
    name: Option<String>,
    rssi: Option<i16>,
}

#[derive(Debug)]
enum Cmd {
    Scan,
    Connect { addr: String },
    Disconnect,
    SetMask(u8),
}

#[derive(Debug)]
enum UiMsg {
    Log(String),
    ScanResults(Vec<DeviceInfo>),
    Connected(bool),
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("com.terence.nrf52840-led-gui")
        .build();

    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &gtk::Application) {
    // GTK -> BLE worker command channel (tokio unbounded)
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Cmd>();

    // BLE worker -> GTK messages (std channel; UI polls it)
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();

    // Spawn BLE worker thread with tokio runtime
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async move {
            if let Err(e) = ble_worker(cmd_rx, ui_tx).await {
                eprintln!("BLE worker error: {e:?}");
            }
        });
    });

    // ===== UI widgets =====
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("nRF52840 BLE LED Controller")
        .default_width(900)
        .default_height(600)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_top(10);
    root.set_margin_bottom(10);
    root.set_margin_start(10);
    root.set_margin_end(10);

    // Top controls row
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let scan_btn = gtk::Button::with_label("Scan");
    let connect_btn = gtk::Button::with_label("Connect");
    let disconnect_btn = gtk::Button::with_label("Disconnect");

    top.append(&scan_btn);
    top.append(&connect_btn);
    top.append(&disconnect_btn);

    // Devices list
    let devices_list = gtk::ListBox::new();
    devices_list.set_selection_mode(gtk::SelectionMode::Single);
    let devices_scroller = gtk::ScrolledWindow::builder()
        .min_content_height(160)
        .child(&devices_list)
        .build();

    // LED controls
    let led_frame = gtk::Frame::builder().label("LEDs").build();
    let led_grid = gtk::Grid::new();
    led_grid.set_row_spacing(8);
    led_grid.set_column_spacing(8);
    led_grid.set_margin_top(8);
    led_grid.set_margin_bottom(8);
    led_grid.set_margin_start(8);
    led_grid.set_margin_end(8);
    led_frame.set_child(Some(&led_grid));

    let led1 = gtk::ToggleButton::with_label("LED1");
    let led2 = gtk::ToggleButton::with_label("LED2");
    let led3 = gtk::ToggleButton::with_label("LED3");
    let led4 = gtk::ToggleButton::with_label("LED4");
    let all_on = gtk::Button::with_label("All On");
    let all_off = gtk::Button::with_label("All Off");

    led_grid.attach(&led1, 0, 0, 1, 1);
    led_grid.attach(&led2, 1, 0, 1, 1);
    led_grid.attach(&led3, 2, 0, 1, 1);
    led_grid.attach(&led4, 3, 0, 1, 1);
    led_grid.attach(&all_on, 0, 1, 2, 1);
    led_grid.attach(&all_off, 2, 1, 2, 1);

    // Log window
    let log_frame = gtk::Frame::builder().label("Log").build();
    let log_view = gtk::TextView::new();
    log_view.set_editable(false);
    log_view.set_monospace(true);
    let log_buf = log_view.buffer();

    let log_scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&log_view)
        .build();
    log_frame.set_child(Some(&log_scroller));

    root.append(&top);
    root.append(&devices_scroller);
    root.append(&led_frame);
    root.append(&log_frame);

    window.set_child(Some(&root));
    window.present();

    // ===== UI state =====
    let devices: Rc<RefCell<Vec<DeviceInfo>>> = Rc::new(RefCell::new(Vec::new()));
    let connected = Rc::new(Cell::new(false));

    set_led_controls_enabled(&[&led1, &led2, &led3, &led4], &all_on, &all_off, false);

    // ===== Button handlers =====
    {
        let cmd_tx = cmd_tx.clone();
        scan_btn.connect_clicked(move |_| {
            let _ = cmd_tx.send(Cmd::Scan);
        });
    }

    {
        let cmd_tx = cmd_tx.clone();
        let devices = devices.clone();
        let devices_list = devices_list.clone();
        connect_btn.connect_clicked(move |_| {
            let row = match devices_list.selected_row() {
                Some(r) => r,
                None => return,
            };
            let idx = row.index();
            if idx < 0 {
                return;
            }
            let devs = devices.borrow();
            let Some(d) = devs.get(idx as usize) else { return; };
            let _ = cmd_tx.send(Cmd::Connect { addr: d.addr.clone() });
        });
    }

    {
        let cmd_tx = cmd_tx.clone();
        disconnect_btn.connect_clicked(move |_| {
            let _ = cmd_tx.send(Cmd::Disconnect);
        });
    }

   // Toggle buttons -> compute mask -> send
{
    let cmd_tx = cmd_tx.clone();
    let connected = connected.clone();

    // clones used INSIDE the send_mask closure
    let led1_for_mask = led1.clone();
    let led2_for_mask = led2.clone();
    let led3_for_mask = led3.clone();
    let led4_for_mask = led4.clone();

    let send_mask = Rc::new(move || {
        let mut m = 0u8;
        if led1_for_mask.is_active() { m |= 0x01; }
        if led2_for_mask.is_active() { m |= 0x02; }
        if led3_for_mask.is_active() { m |= 0x04; }
        if led4_for_mask.is_active() { m |= 0x08; }
        let _ = cmd_tx.send(Cmd::SetMask(m));
    });

    // separate clones used to register signal handlers
    {
        let f = send_mask.clone();
        let led = led1.clone();
        led.connect_toggled(move |_| f());
    }
    {
        let f = send_mask.clone();
        let led = led2.clone();
        led.connect_toggled(move |_| f());
    }
    {
        let f = send_mask.clone();
        let led = led3.clone();
        led.connect_toggled(move |_| f());
    }
    {
        let f = send_mask.clone();
        let led = led4.clone();
        led.connect_toggled(move |_| f());
    }
}
    // All On
{
    let cmd_tx = cmd_tx.clone();
    let led1 = led1.clone();
    let led2 = led2.clone();
    let led3 = led3.clone();
    let led4 = led4.clone();

    all_on.connect_clicked(move |_| {
        led1.set_active(true);
        led2.set_active(true);
        led3.set_active(true);
        led4.set_active(true);
        let _ = cmd_tx.send(Cmd::SetMask(0x0f));
    });
}

// All Off
{
    let cmd_tx = cmd_tx.clone();
    let led1 = led1.clone();
    let led2 = led2.clone();
    let led3 = led3.clone();
    let led4 = led4.clone();

    all_off.connect_clicked(move |_| {
        led1.set_active(false);
        led2.set_active(false);
        led3.set_active(false);
        led4.set_active(false);
        let _ = cmd_tx.send(Cmd::SetMask(0x00));
    });
}


    // ===== UI poller: pump UiMsg from std::mpsc into GTK =====
    {
        let devices = devices.clone();
        let devices_list = devices_list.clone();
        let connected_state = connected.clone();

        let log_buf = log_buf.clone();
        let log_view = log_view.clone();

        let led1 = led1.clone();
        let led2 = led2.clone();
        let led3 = led3.clone();
        let led4 = led4.clone();
        let all_on = all_on.clone();
        let all_off = all_off.clone();

        gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
            while let Ok(msg) = ui_rx.try_recv() {
                match msg {
                    UiMsg::Log(line) => append_log(&log_buf, &log_view, &line),

                    UiMsg::ScanResults(list) => {
                        devices.replace(list);

                        // Clear listbox (GTK4: remove children manually)
                        while let Some(child) = devices_list.first_child() {
                            devices_list.remove(&child);
                        }

                        for d in devices.borrow().iter() {
                            let name = d.name.clone().unwrap_or_else(|| "(no name)".into());
                            let rssi = d.rssi.map(|v| format!("{v} dBm")).unwrap_or_else(|| "? dBm".into());
                            let label = gtk::Label::new(Some(&format!("{name}  |  {}  |  {rssi}", d.addr)));
                            label.set_xalign(0.0);

                            let row = gtk::ListBoxRow::new();
                            row.set_child(Some(&label));
                            devices_list.append(&row);
                        }

                        append_log(&log_buf, &log_view, &format!("Scan results: {} device(s)", devices.borrow().len()));
                    }

                    UiMsg::Connected(is_connected) => {
                        connected_state.set(is_connected);
                        append_log(&log_buf, &log_view, if is_connected { "Connected." } else { "Disconnected." });

                        set_led_controls_enabled(
                            &[&led1, &led2, &led3, &led4],
                            &all_on,
                            &all_off,
                            is_connected,
                        );
                    }
                }
            }

            gtk::glib::ControlFlow::Continue
        });
    }

    append_log(&log_buf, &log_view, "Ready. Click Scan.");
}

fn set_led_controls_enabled(
    toggles: &[&gtk::ToggleButton],
    all_on: &gtk::Button,
    all_off: &gtk::Button,
    enabled: bool,
) {
    for t in toggles {
        t.set_sensitive(enabled);
    }
    all_on.set_sensitive(enabled);
    all_off.set_sensitive(enabled);
}

fn append_log(buf: &gtk::TextBuffer, view: &gtk::TextView, line: &str) {
    let mut text = line.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }

    let mut end = buf.end_iter();
    buf.insert(&mut end, &text);

    let end2 = buf.end_iter();
    let mark = buf.create_mark(None, &end2, false);
    view.scroll_mark_onscreen(&mark);
}

async fn ble_worker(
    mut rx: tokio_mpsc::UnboundedReceiver<Cmd>,
    ui_tx: mpsc::Sender<UiMsg>,
) -> Result<()> {
    let manager = Manager::new().await.context("btleplug Manager::new")?;
    let adapters = manager.adapters().await.context("manager.adapters")?;
    let adapter = adapters.into_iter().next().ok_or_else(|| anyhow!("No BLE adapters found"))?;

    let _ = ui_tx.send(UiMsg::Log("BLE worker started.".into()));

    let mut last_scan: Vec<(DeviceInfo, Peripheral)> = Vec::new();
    let mut connected: Option<(Peripheral, btleplug::api::Characteristic)> = None;
    let led_uuid = Uuid::parse_str(LED_CHAR_UUID).unwrap();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Scan => {
                let _ = ui_tx.send(UiMsg::Log("Scanning (5s)...".into()));
                adapter.start_scan(ScanFilter::default()).await.context("start_scan")?;
                tokio::time::sleep(Duration::from_secs(5)).await;

                let (infos, peris) = collect_devices(&adapter).await?;
                last_scan = infos.into_iter().zip(peris.into_iter()).collect();

                let just_infos: Vec<DeviceInfo> = last_scan.iter().map(|(i, _)| i.clone()).collect();
                let _ = ui_tx.send(UiMsg::ScanResults(just_infos));
            }

            Cmd::Connect { addr } => {
                let _ = ui_tx.send(UiMsg::Log(format!("Connect requested: {addr}")));

                let Some((_, peri)) = last_scan.iter().find(|(i, _)| i.addr == addr).cloned()
                else {
                    let _ = ui_tx.send(UiMsg::Log("That device wasn't in the last scan list.".into()));
                    continue;
                };

                peri.connect().await.context("peripheral.connect")?;
                peri.discover_services().await.context("discover_services")?;

                let chars = peri.characteristics();
                let Some(ch) = chars.into_iter().find(|c| c.uuid == led_uuid) else {
                    let _ = ui_tx.send(UiMsg::Log("LED characteristic not found on device.".into()));
                    peri.disconnect().await.ok();
                    let _ = ui_tx.send(UiMsg::Connected(false));
                    continue;
                };

                if !(ch.properties.contains(CharPropFlags::WRITE)
                    || ch.properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE))
                {
                    let _ = ui_tx.send(UiMsg::Log(
                        "Warning: LED characteristic doesn't advertise WRITE; attempting anyway.".into(),
                    ));
                }

                connected = Some((peri, ch));
                let _ = ui_tx.send(UiMsg::Connected(true));
            }

            Cmd::Disconnect => {
                if let Some((peri, _)) = connected.take() {
                    let _ = ui_tx.send(UiMsg::Log("Disconnecting...".into()));
                    peri.disconnect().await.ok();
                }
                let _ = ui_tx.send(UiMsg::Connected(false));
            }

            Cmd::SetMask(m) => {
                if let Some((peri, ch)) = &connected {
                    let data = [m];
                    match peri.write(ch, &data, WriteType::WithResponse).await {
                        Ok(_) => {
                            let _ = ui_tx.send(UiMsg::Log(format!("Wrote LED mask: 0x{m:02x}")));
                        }
                        Err(e) => {
                            let _ = ui_tx.send(UiMsg::Log(format!("Write failed: {e:?}")));
                        }
                    }
                } else {
                    let _ = ui_tx.send(UiMsg::Log("Not connected; ignoring LED write.".into()));
                }
            }
        }
    }

    Ok(())
}

async fn collect_devices(adapter: &Adapter) -> Result<(Vec<DeviceInfo>, Vec<Peripheral>)> {
    let peris = adapter.peripherals().await.context("adapter.peripherals")?;
    let mut infos = Vec::new();
    let mut keep = Vec::new();

    for p in peris {
        let props = p.properties().await.ok().flatten();
        let addr = p.id().to_string();
        let name = props.as_ref().and_then(|x| x.local_name.clone());
        let rssi = props.as_ref().and_then(|x| x.rssi);

        infos.push(DeviceInfo { addr, name, rssi });
        keep.push(p);
    }

    // Sort: named first, stronger RSSI first
    let mut zipped: Vec<(DeviceInfo, Peripheral)> = infos.into_iter().zip(keep.into_iter()).collect();
    zipped.sort_by(|a, b| {
        let an = a.0.name.is_some();
        let bn = b.0.name.is_some();
        bn.cmp(&an)
            .then_with(|| b.0.rssi.unwrap_or(-999).cmp(&a.0.rssi.unwrap_or(-999)))
    });

    let (infos2, peris2): (Vec<_>, Vec<_>) = zipped.into_iter().unzip();
    Ok((infos2, peris2))
}

