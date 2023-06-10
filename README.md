# ESP32 w/ 5.83" E-Ink Waveshare display

## Configuration
1) Register your thing on AWS IoT with the correct policy and download the required certificates, along with the AWS root CA certificate.
2) Place them in the certificates folder, ensuring the filenames match with the AWS IoT certificate paths inside main.rs.
3) Set your AWS IoT MQTT endpoint in main.rs (MQTT_ENDPOINT).
4) Configure your WiFi credentials in main.rs.

## Flash
See https://esp-rs.github.io/book/tooling/espflash.html for details
```sh
cargo espflash --release --monitor --partition-table partition-table.csv
```

