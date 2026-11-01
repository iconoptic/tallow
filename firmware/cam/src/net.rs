//! Join `tallow-iot` as `toby-cam-0`.
//!
//! WPA2 is required. The address is static: `10.87.0.23/24`, gateway and DNS
//! `10.87.0.1`. There is no DHCP client and no access-point configuration, so
//! a failed join does not open a fallback network. Modem sleep stays on.
//!
//! The SSID and the password are baked by `build.rs` from
//! `firmware/esphome/secrets.yaml`. They are not printed.

use core::str;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::ipv4::{
    ClientConfiguration as Ipv4Client, ClientSettings, Configuration as Ipv4Configuration, Mask,
    Subnet,
};
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{self, EspError};
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi, WifiDriver,
};
use std::net::Ipv4Addr;

const SSID: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wifi_ssid.bin"));
const PASSWORD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wifi_password.bin"));

/// A joined station. Dropping it stops the radio, so the main loop holds it.
pub struct Link {
    wifi: BlockingWifi<EspWifi<'static>>,
}

impl Link {
    /// True while the station is associated and its interface is up.
    pub fn up(&self) -> bool {
        self.wifi.is_up().unwrap_or(false)
    }

    /// Try once to associate again. The caller waits between tries.
    pub fn reconnect(&mut self) -> Result<(), EspError> {
        self.wifi.connect()?;
        self.wifi.wait_netif_up()?;
        modem_sleep()
    }
}

/// Start the station and return it once `10.87.0.23` is up.
/// A bad bake spins here. It does not join an open network.
pub fn join(nvs: Option<EspDefaultNvsPartition>) -> Link {
    let config = match client_config() {
        Ok(config) => config,
        Err(msg) => {
            println!("{msg}");
            halt();
        }
    };
    let mut wifi = match start(config, nvs) {
        Ok(wifi) => wifi,
        Err(err) => {
            println!("wifi start failed: {err}");
            halt();
        }
    };
    loop {
        match connect(&mut wifi) {
            Ok(()) => {
                println!("wifi up");
                return Link { wifi };
            }
            Err(err) => {
                println!("wifi join failed: {err}");
                FreeRtos::delay_ms(1000);
            }
        }
    }
}

fn client_config() -> Result<ClientConfiguration, &'static str> {
    let ssid = str::from_utf8(SSID).map_err(|_| "wifi credentials are not utf-8")?;
    let password = str::from_utf8(PASSWORD).map_err(|_| "wifi credentials are not utf-8")?;
    if ssid.is_empty() || password.is_empty() {
        return Err("wifi credentials are empty");
    }
    Ok(ClientConfiguration {
        ssid: ssid.try_into().map_err(|_| "wifi ssid does not fit")?,
        password: password
            .try_into()
            .map_err(|_| "wifi password does not fit")?,
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    })
}

fn start(
    config: ClientConfiguration,
    nvs: Option<EspDefaultNvsPartition>,
) -> Result<BlockingWifi<EspWifi<'static>>, EspError> {
    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let driver = WifiDriver::new(peripherals.modem, sysloop.clone(), nvs)?;
    // Hand over the static interface here. `EspWifi::wrap` builds a default
    // station interface first, and a second one with the same `WIFI_STA_DEF`
    // key is refused by `esp_netif_new`, so swapping afterwards cannot work.
    // `wrap_all` takes an access-point interface as well because softAP
    // support is compiled in. The configuration below is client-only, so that
    // interface is never started, and no DHCP server runs.
    let esp = EspWifi::wrap_all(
        driver,
        EspNetif::new_with_conf(&sta_netif())?,
        EspNetif::new(NetifStack::Ap)?,
    )?;
    let mut wifi = BlockingWifi::wrap(esp, sysloop)?;
    wifi.set_configuration(&Configuration::Client(config))?;
    wifi.start()?;
    modem_sleep()?;
    Ok(wifi)
}

fn sta_netif() -> NetifConfiguration {
    let mut conf = NetifConfiguration::wifi_default_client();
    conf.ip_configuration = Some(Ipv4Configuration::Client(Ipv4Client::Fixed(
        ClientSettings {
            ip: Ipv4Addr::new(10, 87, 0, 23),
            subnet: Subnet {
                gateway: Ipv4Addr::new(10, 87, 0, 1),
                mask: Mask(24),
            },
            dns: Some(Ipv4Addr::new(10, 87, 0, 1)),
            secondary_dns: None,
        },
    )));
    conf
}

fn connect(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<(), EspError> {
    wifi.connect()?;
    wifi.wait_netif_up()?;
    modem_sleep()
}

/// `WIFI_PS_MIN_MODEM`: the station stays associated and wakes for every DTIM
/// beacon. That is the ESP-IDF default, and the sleep the ESPHome image used.
fn modem_sleep() -> Result<(), EspError> {
    // SAFETY: esp_wifi_set_ps takes a power-save mode, not a pointer. Wi-Fi is
    // already started, which is when ESP-IDF allows this call. The mode is the
    // MIN_MODEM constant, not a value from the air.
    sys::esp!(unsafe { sys::esp_wifi_set_ps(sys::wifi_ps_type_t_WIFI_PS_MIN_MODEM) })
}

fn halt() -> ! {
    loop {
        FreeRtos::delay_ms(1000);
    }
}
