# nRF52840 BLE LED Controller (Rust + Linux GUI)

This repo is a learning-friendly example that shows a full BLE loop:

1) **nRF52840-DK firmware** advertises over BLE and exposes a **writable GATT characteristic**.
2) A **Linux GUI (Rust + GTK4)** scans for devices, connects, and writes a **1-byte LED bitmask** to control the DK LEDs.

If you’re learning BLE + Rust, this is a nice “small but real” project:
- GATT service + characteristic on the embedded side
- Scan/connect/write on the Linux side
- A GUI and logging so you can see what’s happening

---

## Repo layout

- `firmware/`  
  Embedded Rust firmware (nRF52840-DK) using `embassy` + `nrf-softdevice`.

- `gui/nrf52840_led_gui/`  
  Rust GTK4 application using `btleplug` (BlueZ backend on Linux) to control LEDs.

---

## What the BLE interface looks like

The firmware exposes a writable characteristic:

- **LED characteristic UUID**:  
  `9e7312e0-2354-11eb-9f10-fbc30a63cf38`

The GUI writes a **single byte** where each bit controls one LED:

| Value (hex) | Meaning |
|---|---|
| `0x00` | all off |
| `0x01` | LED1 |
| `0x02` | LED2 |
| `0x04` | LED3 |
| `0x08` | LED4 |
| `0x0F` | all on |

This matches how you tested manually with `bluetoothctl`.

---

## Prerequisites (Linux)

### Hardware
- Nordic **nRF52840-DK**

### Software (Arch Linux example)
```bash
sudo pacman -S --needed bluez bluez-utils gtk4 pkgconf
sudo systemctl enable --now bluetooth

