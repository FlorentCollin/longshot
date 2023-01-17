use std::time::Duration;

use btleplug::api::{Central, Characteristic, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Manager, Peripheral};
use uuid::Uuid;

const SERVICE_UUID: Uuid = Uuid::from_u128(0x00035b03_58e6_07dd_021a_08123a000300);
const CHARACTERISTIC_UUID: Uuid = Uuid::from_u128(0x00035b03_58e6_07dd_021a_08123a000301);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manager = Manager::new().await?;
    let filter = ScanFilter {
        services: vec![SERVICE_UUID],
    };

    eprintln!("Looking for coffeemakers...");
    for adapter in manager.adapters().await? {
        adapter.start_scan(filter.clone()).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        for peripheral in adapter.peripherals().await? {
            eprintln!("Found peripheral: {:?}", peripheral.id());
            peripheral.connect().await?;
            peripheral.discover_services().await?;
            for service in peripheral.services() {
                for characteristic in service.characteristics {
                    if service.uuid == SERVICE_UUID && characteristic.uuid == CHARACTERISTIC_UUID {
                        run_with_peripheral(peripheral.clone(), characteristic).await?;
                    }
                }
            }
        }
    }

    Ok(())
}

async fn run_with_peripheral(
    peripheral: Peripheral,
    characteristic: Characteristic,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = &[
        0x0d, 0x14, 0x83, 0xf0, 0x07, 0x01, 0x01, 0x00, 0x41, 0x09, 0x00, 0xbe, 0x02, 0x03, 0x0c,
        0x00, 0x1c, 0x02, 0x06, 0xdc,
    ];
    loop {
        println!("Sending request");
        let result = peripheral
            .write(
                &characteristic,
                data,
                btleplug::api::WriteType::WithoutResponse,
            )
            .await;
        match result {
            Ok(_) => break,
            Err(error) => {
                println!("{:?}", error);
            }
        }
    }
    Ok(())
}
