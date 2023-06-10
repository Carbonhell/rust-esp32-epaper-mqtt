use embedded_graphics::{
    mono_font::MonoTextStyleBuilder,
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, Point},
    text::{Baseline, Text, TextStyleBuilder},
    Drawable,
};
use embedded_svc::{
    mqtt::client::{Connection, Event, Message, MessageImpl, QoS},
    utils::mqtt::client::ConnState,
    wifi::{AuthMethod, ClientConfiguration, Configuration},
};
use epd_waveshare::{
    buffer_len,
    epd5in83_v2::{self, Display5in83, Epd5in83},
    graphics::VarDisplay,
    prelude::{Color, Display, DisplayRotation, TriColor, WaveshareDisplay},
};
use esp_idf_hal::prelude::*;
use esp_idf_hal::{
    delay::{Delay, Ets},
    gpio::{AnyIOPin, Gpio2, PinDriver},
    prelude::Peripherals,
    spi::{config::Config, SpiDeviceDriver, SpiDriverConfig},
};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    mqtt::client::{EspMqttClient, MqttClientConfiguration},
    nvs::EspDefaultNvsPartition,
    tls::X509,
    wifi::{BlockingWifi, EspWifi},
};
use esp_idf_sys::{self as _, EspError}; // If using the `binstart` feature of `esp-idf-sys`, always keep this module imported
use log::*;
use std::{
    mem, slice,
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
};

// WiFi configuration
const WIFI_SSID: &str = "";
const WIFI_PASS: &str = "";

// MQTT configuration. Not specific to AWS IoT but currently certificates aren't optional. If you want to use Emqx instead of AWS IoT, see https://www.emqx.com/en/blog/emqx-server-ssl-tls-secure-connection-configuration-guide
const MQTT_ENDPOINT: &str = "YOUR_AWS_IOT_MQTT_ENDPOINT_HERE";
const MQTT_CLIENT_ID: &str = "esp32-epaper-main";
const MQTT_TOPIC_NAME: &str = "topic/sdk/test/rust";

// AWS IoT certificate
const CA_CERT_PATH: &str = "../certificates/AmazonRootCA1.pem";
const THING_CERT_PATH: &str = "../certificates/esp32-epaper-main.client.crt";
const THING_PRIVATE_KEY_PATH: &str = "../certificates/esp32-epaper-main.private.key";

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_sys::link_patches();
    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take().unwrap();
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    Delay::delay_ms(3000);
    // Blocking so that we can block until the IP is obtained
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;

    configure_wifi(&mut wifi)?;

    Delay::delay_ms(3000);

    info!("Configuring the E-Ink display...");
    let mut display = Display5in83::default();

    let spi = peripherals.spi2;

    // Firebeetle pins
    let sclk = peripherals.pins.gpio18;
    let serial_out = peripherals.pins.gpio23;
    let cs = PinDriver::output(peripherals.pins.gpio14)?;
    let busy_in = PinDriver::input(peripherals.pins.gpio4)?;
    let dc = PinDriver::output(peripherals.pins.gpio22)?;
    let rst = PinDriver::output(peripherals.pins.gpio21)?;

    let config = Config::new().baudrate(112500.into());
    let mut device = SpiDeviceDriver::new_single(
        spi,
        sclk,
        serial_out,
        Option::<Gpio2>::None,
        Option::<AnyIOPin>::None,
        &SpiDriverConfig::default(),
        &config,
    )?;

    let mut delay = Ets;

    Delay::delay_ms(3000);
    let mut epd = Epd5in83::new(&mut device, cs, busy_in, dc, rst, &mut delay, None)?;
    info!("E-Ink display init completed!");

    //Set up a channel to send messages received from the MQTT queue (separate thread) to the main thread, to display them on the e-paper module
    info!("Setting up the MQTT client...");
    let (sender, receiver) = mpsc::channel::<String>();
    let _mqtt_client: EspMqttClient<ConnState<MessageImpl, EspError>> = setup_mqtt_client(sender)?;

    loop {
        Delay::delay_ms(3000);
        // Check for new messages every 3 seconds for 2 seconds
        let message = receiver.recv_timeout(Duration::from_millis(2000));
        if let Ok(message) = message {
            info!("Message received in main thread: {:?}", message);
            display.clear(Color::White)?;
            draw_text(&mut display, &message, 0, 0);
            epd.update_frame(&mut device, display.buffer(), &mut delay)?;
            epd.display_frame(&mut device, &mut delay)?;
        }
    }
}

fn configure_wifi(wifi: &mut BlockingWifi<EspWifi>) -> Result<(), EspError> {
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.into(),
        password: WIFI_PASS.into(),
        auth_method: AuthMethod::None,
        ..Default::default()
    }))?;
    wifi.start()?;
    info!("Wifi started!");

    wifi.connect()?;
    info!("Wifi connected!");

    wifi.wait_netif_up()?;
    info!("Wifi ready!");

    Ok(())
}

fn setup_mqtt_client(
    sender: Sender<String>,
) -> Result<EspMqttClient<ConnState<MessageImpl, EspError>>, EspError> {
    info!("About to start MQTT client");

    let server_cert_bytes: Vec<u8> = include_bytes!(CA_CERT_PATH).to_vec();
    let client_cert_bytes: Vec<u8> = include_bytes!(THING_CERT_PATH).to_vec();
    let private_key_bytes: Vec<u8> = include_bytes!(THING_PRIVATE_KEY_PATH).to_vec();

    let server_cert: X509 = convert_certificate(server_cert_bytes);
    let client_cert: X509 = convert_certificate(client_cert_bytes);
    let private_key: X509 = convert_certificate(private_key_bytes);

    let conf = MqttClientConfiguration {
        client_id: Some(MQTT_CLIENT_ID),
        crt_bundle_attach: Some(esp_idf_sys::esp_crt_bundle_attach),
        server_certificate: Some(server_cert),
        client_certificate: Some(client_cert),
        private_key: Some(private_key),
        ..Default::default()
    };
    let (mut client, mut connection) = EspMqttClient::new_with_conn(MQTT_ENDPOINT, &conf)?;

    info!("MQTT client started!");

    thread::spawn(move || {
        info!("MQTT Listening for messages...");

        // Send received messages back to the main thread to display them
        while let Some(msg) = connection.next() {
            match msg {
                Err(e) => info!("MQTT Message ERROR: {}", e),
                Ok(msg) => {
                    info!("MQTT Message: {:?}", msg);
                    if let Event::Received(msg) = msg {
                        let parsed_string = String::from_utf8(msg.data().to_vec());
                        if let Ok(parsed_string) = parsed_string {
                            info!("Parsed MQTT message: {:?}", parsed_string);
                            sender.send(parsed_string).unwrap();
                        }
                    }
                }
            }
        }

        info!("MQTT connection loop exit");
    });

    client.subscribe(MQTT_TOPIC_NAME, QoS::AtMostOnce)?;

    info!("Subscribed to all topics ({})", MQTT_TOPIC_NAME);

    Delay::delay_ms(1000);
    // This will be the first message appearing on the screen
    client.publish(
        MQTT_TOPIC_NAME,
        QoS::AtMostOnce,
        false,
        format!("Hello from {}!", MQTT_TOPIC_NAME).as_bytes(),
    )?;

    info!(
        "Published a hello message to topic \"{}\".",
        MQTT_TOPIC_NAME
    );

    Ok(client)
}

fn convert_certificate(mut certificate_bytes: Vec<u8>) -> X509<'static> {
    // append NUL
    certificate_bytes.push(0);

    // convert the certificate
    let certificate_slice: &[u8] = unsafe {
        let ptr: *const u8 = certificate_bytes.as_ptr();
        let len: usize = certificate_bytes.len();
        mem::forget(certificate_bytes);

        slice::from_raw_parts(ptr, len)
    };

    // return the certificate file in the correct format
    X509::pem_until_nul(certificate_slice)
}

pub fn draw_text(display: &mut Display5in83, text: &str, x: i32, y: i32) {
    let style = MonoTextStyleBuilder::new()
        .font(&embedded_graphics::mono_font::ascii::FONT_10X20)
        .text_color(Color::White)
        .background_color(Color::Black)
        .build();

    let text_style = TextStyleBuilder::new().baseline(Baseline::Top).build();

    let _ = Text::with_text_style(text, Point::new(x, y), style, text_style).draw(display);
}
