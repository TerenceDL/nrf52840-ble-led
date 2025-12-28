#![no_std]
#![no_main]

#[path = "../example_common.rs"]
mod example_common;

use core::mem;

use defmt::{info, warn, *};
use embassy_executor::Spawner;
use embassy_nrf::{
    config,
    gpio::{AnyPin, Level, Output, OutputDrive},
    interrupt::Priority,
};
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList, ServiceUuid16,
};
use nrf_softdevice::ble::{gatt_server, peripheral};
use nrf_softdevice::{raw, Softdevice};

#[embassy_executor::task]
async fn softdevice_task(sd: &'static Softdevice) -> ! {
    sd.run().await
}

#[nrf_softdevice::gatt_service(uuid = "180f")]
struct BatteryService {
    #[characteristic(uuid = "2a19", read, notify)]
    battery_level: u8,
}

/// Custom LED control service: write 1 byte bitmask.
/// bit0..bit3 => LED1..LED4
#[nrf_softdevice::gatt_service(uuid = "9e7312e0-2354-11eb-9f10-fbc30a62cf38")]
struct LedService {
    #[characteristic(uuid = "9e7312e0-2354-11eb-9f10-fbc30a63cf38", read, write, notify)]
    led_mask: u8,
}

#[nrf_softdevice::gatt_server]
struct Server {
    bas: BatteryService,
    led: LedService,
}

struct Leds {
    // embassy-nrf 0.3.1 Output is Output<'d> (no pin generic). Use AnyPin.
    led1: Output<'static>,
    led2: Output<'static>,
    led3: Output<'static>,
    led4: Output<'static>,
}

impl Leds {
    fn new(p: embassy_nrf::Peripherals) -> Self {
        // nRF52840-DK LEDs are P0.13..P0.16 and are active-low.
        // Start HIGH = off.
        let led1 = Output::new(AnyPin::from(p.P0_13), Level::High, OutputDrive::Standard);
        let led2 = Output::new(AnyPin::from(p.P0_14), Level::High, OutputDrive::Standard);
        let led3 = Output::new(AnyPin::from(p.P0_15), Level::High, OutputDrive::Standard);
        let led4 = Output::new(AnyPin::from(p.P0_16), Level::High, OutputDrive::Standard);

        Self {
            led1,
            led2,
            led3,
            led4,
        }
    }

    fn all_off(&mut self) {
        self.led1.set_high();
        self.led2.set_high();
        self.led3.set_high();
        self.led4.set_high();
    }

    fn apply_mask(&mut self, mask: u8) {
        // active-low: LOW = ON, HIGH = OFF
        if (mask & 0x01) != 0 {
            self.led1.set_low();
        } else {
            self.led1.set_high();
        }
        if (mask & 0x02) != 0 {
            self.led2.set_low();
        } else {
            self.led2.set_high();
        }
        if (mask & 0x04) != 0 {
            self.led3.set_low();
        } else {
            self.led3.set_high();
        }
        if (mask & 0x08) != 0 {
            self.led4.set_low();
        } else {
            self.led4.set_high();
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("Hello World!");

    // Initialize Embassy peripherals with interrupt priorities compatible with SoftDevice.
    // Using Default::default() here can trigger SdmIncorrectInterruptConfiguration on enable().
    let mut ecfg = config::Config::default();
    ecfg.gpiote_interrupt_priority = Priority::P3;
    ecfg.time_interrupt_priority = Priority::P3;

    let p = embassy_nrf::init(ecfg);

    let mut leds = Leds::new(p);
    leds.all_off();

    let config = nrf_softdevice::Config {
        clock: Some(raw::nrf_clock_lf_cfg_t {
            source: raw::NRF_CLOCK_LF_SRC_RC as u8,
            rc_ctiv: 16,
            rc_temp_ctiv: 2,
            accuracy: raw::NRF_CLOCK_LF_ACCURACY_500_PPM as u8,
        }),
        conn_gap: Some(raw::ble_gap_conn_cfg_t {
            conn_count: 6,
            event_length: 24,
        }),
        conn_gatt: Some(raw::ble_gatt_conn_cfg_t { att_mtu: 256 }),
        gatts_attr_tab_size: Some(raw::ble_gatts_cfg_attr_tab_size_t {
            attr_tab_size: raw::BLE_GATTS_ATTR_TAB_SIZE_DEFAULT,
        }),
        gap_role_count: Some(raw::ble_gap_cfg_role_count_t {
            adv_set_count: 1,
            periph_role_count: 3,
            central_role_count: 3,
            central_sec_count: 0,
            _bitfield_1: raw::ble_gap_cfg_role_count_t::new_bitfield_1(0),
        }),
        gap_device_name: Some(raw::ble_gap_cfg_device_name_t {
            p_value: b"HelloRust" as *const u8 as _,
            current_len: 9,
            max_len: 9,
            write_perm: unsafe { mem::zeroed() },
            _bitfield_1: raw::ble_gap_cfg_device_name_t::new_bitfield_1(raw::BLE_GATTS_VLOC_STACK as u8),
        }),
        ..Default::default()
    };

    let sd = Softdevice::enable(&config);
    let server = unwrap!(Server::new(sd));
    unwrap!(spawner.spawn(softdevice_task(sd)));

    static ADV_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .flags(&[Flag::GeneralDiscovery, Flag::LE_Only])
        .services_16(ServiceList::Complete, &[ServiceUuid16::BATTERY])
        .full_name("HelloRust")
        .build();

    static SCAN_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .services_128(
            ServiceList::Complete,
            &[0x9e7312e0_2354_11eb_9f10_fbc30a62cf38_u128.to_le_bytes()],
        )
        .build();

    loop {
        let config = peripheral::Config::default();
        let adv = peripheral::ConnectableAdvertisement::ScannableUndirected {
            adv_data: &ADV_DATA,
            scan_data: &SCAN_DATA,
        };
        let conn = unwrap!(peripheral::advertise_connectable(sd, adv, &config).await);

        info!("connected!");

        let r = gatt_server::run(&conn, &server, |e| match e {
            ServerEvent::Bas(e) => match e {
                BatteryServiceEvent::BatteryLevelCccdWrite { notifications } => {
                    info!("battery notifications: {}", notifications)
                }
            },

            ServerEvent::Led(e) => match e {
                LedServiceEvent::LedMaskWrite(mask) => {
                    info!("LED mask write: 0x{:02x}", mask);
                    leds.apply_mask(mask);

                    // Optional: notify back current mask so PC can confirm state.
                    if let Err(err) = server.led.led_mask_notify(&conn, &mask) {
                        warn!("notify led_mask failed: {:?}", err);
                    }
                }
                LedServiceEvent::LedMaskCccdWrite { notifications } => {
                    info!("led notifications: {}", notifications)
                }
            },
        })
        .await;

        info!("disconnected: {:?}", r);
        leds.all_off();
    }
}


