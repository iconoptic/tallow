//! Join `tallow-iot`.
//!
//! WPA2 is required. Every cam runs this one image, so the address comes from
//! DHCP: `tools/mesh.sh cam-flash` reserves one address per cam MAC with
//! `tools/setup_iot_ap.sh reserve`, and dnsmasq hands it out. The DHCP
//! hostname is `tallow-cam-` and the last three MAC bytes. There is no
//! access-point configuration, so a failed join does not open a fallback
//! network. Modem sleep stays off.
//!
//! The SSID and the password are baked by `build.rs` from
//! `firmware/cam/secrets.yaml`. They are not printed.

use core::str;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::ipv4::{
    ClientConfiguration as Ipv4Client, Configuration as Ipv4Configuration, DHCPClientSettings,
};
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{self, EspError};
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi, WifiDriver,
};

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
        stay_awake()
    }

    /// The driver still has an access-point record. After the AP drops the
    /// station, this becomes false even when [`Link::up`] has not noticed yet.
    pub fn heard(&self) -> bool {
        // SAFETY: `info` is zeroed storage owned by this call. The getter writes
        // that record and does not keep the pointer. Wi-Fi is already started.
        // A failure means there is no access point.
        unsafe {
            let mut info: sys::wifi_ap_record_t = core::mem::zeroed();
            sys::esp_wifi_sta_get_ap_info(&mut info) == sys::ESP_OK
        }
    }
}

/// Start the station and return it once DHCP has given it an address.
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
    // Hand over the named interface here. `EspWifi::wrap` builds a default
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
    stay_awake()?;
    Ok(wifi)
}

fn sta_netif() -> NetifConfiguration {
    let mut conf = NetifConfiguration::wifi_default_client();
    let name = hostname(station_mac());
    conf.ip_configuration = Some(Ipv4Configuration::Client(Ipv4Client::DHCP(
        DHCPClientSettings {
            hostname: name.as_str().try_into().ok(),
        },
    )));
    conf
}

/// `tallow-cam-` and the last three bytes of the station MAC, or `tallow-cam`
/// when the MAC could not be read.
fn hostname(mac: Option<[u8; 6]>) -> String {
    match mac {
        Some(m) => format!("tallow-cam-{:02x}{:02x}{:02x}", m[3], m[4], m[5]),
        None => "tallow-cam".to_owned(),
    }
}

fn station_mac() -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    // SAFETY: `mac` is six bytes owned by this call, the size ESP-IDF writes
    // for a station MAC. The getter reads eFuse and does not keep the pointer.
    let code = unsafe { sys::esp_read_mac(mac.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_WIFI_STA) };
    (code == sys::ESP_OK).then_some(mac)
}

fn connect(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<(), EspError> {
    wifi.connect()?;
    wifi.wait_netif_up()?;
    stay_awake()
}

/// The station stays awake. On 2026-11-02 the access point removed
/// `44:bd:8d:7a:51:60` with "disassociated due to inactivity" while this
/// image was in modem sleep, and the station never associated again.
/// This camera has no Bluetooth, so modem sleep is not required for coexistence.
fn stay_awake() -> Result<(), EspError> {
    // SAFETY: esp_wifi_set_ps takes a power-save mode, not a pointer. Wi-Fi is
    // already started, which is when ESP-IDF allows this call. NONE is the
    // mode constant, not a value from the air.
    sys::esp!(unsafe { sys::esp_wifi_set_ps(sys::wifi_ps_type_t_WIFI_PS_NONE) })
}

fn halt() -> ! {
    loop {
        FreeRtos::delay_ms(1000);
    }
}
