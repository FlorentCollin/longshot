use crate::ecam::{EcamDriver, EcamDriverOutput, EcamError, EcamPacketReceiver};
use crate::{prelude::*, protocol::*};
use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter,
};
use btleplug::platform::{Adapter, Manager};
use stream_cancel::{StreamExt as _, Tripwire};
use tokio::time;
use uuid::Uuid;

use super::packet_stream::packet_stream;

const SERVICE_UUID: Uuid = Uuid::from_u128(0x00035b03_58e6_07dd_021a_08123a000300);
const CHARACTERISTIC_UUID: Uuid = Uuid::from_u128(0x00035b03_58e6_07dd_021a_08123a000301);

/// The concrete peripheral type to avoid going crazy here managaing an unsized trait.
type Peripheral = <Adapter as Central>::Peripheral;

/// Bluetooth implementation of [`EcamDriver`], running on top of [`btleplug`].
pub struct EcamBT {
    peripheral: EcamPeripheral,
    notifications: EcamPacketReceiver,
}

impl EcamBT {
    /// Returns the given [`EcamBT`] instance identified by the [`Uuid`].
    pub async fn get(uuid: String) -> Result<Self, EcamError> {
        let manager = Manager::new().await?;
        Self::get_ecam_from_manager(&manager, uuid).await
    }

    async fn get_ecam_from_manager(manager: &Manager, uuid: String) -> Result<Self, EcamError> {
        let adapter_list = manager.adapters().await?;
        if adapter_list.is_empty() {
            return Result::Err(EcamError::NotFound);
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        for adapter in adapter_list.into_iter() {
            adapter.start_scan(ScanFilter::default()).await?;
            let tx = tx.clone();
            let uuid = uuid.clone();
            let _ = tokio::spawn(async move {
                trace_packet!("Looking for peripheral {}", uuid);
                loop {
                    let peripherals = adapter.peripherals().await?;
                    let mut peripheral = None;
                    if peripherals.is_empty() {
                        println!("There is no peripherals...")
                    }
                    for periph in peripherals.iter() {
                        println!("Found peripheral with id: {:?}", { periph.id() });
                        if periph.id().to_string() == uuid {
                            peripheral = Some(periph);
                        }
                    }
                    // let peripheral = EcamBT::get_ecam_from_adapter(&adapter).await?;
                    if let Some(peripheral) = peripheral {
                        trace_packet!("Got peripheral");
                        let peripheral = EcamPeripheral::connect(peripheral.clone()).await?;
                        trace_packet!("Connected");
                        let notifications = EcamPacketReceiver::from_stream(
                            Box::pin(peripheral.notifications().await?),
                            true,
                        );
                        trace_packet!("Notifications variable set");

                        // Ignore errors here -- we just want the first peripheral that connects
                        let _ = tx
                            .send(EcamBT {
                                peripheral,
                                notifications,
                            })
                            .await;
                        trace_packet!("Message send correctly :)");
                        break;
                    } else {
                        return Result::Err(EcamError::NotFound);
                    }
                }
                Result::<_, EcamError>::Ok(())
            })
            .await;
        }

        Ok(rx.recv().await.expect("Failed to receive anything"))
    }

    /// Scans for ECAM devices.
    async fn scan() -> Result<(String, String), EcamError> {
        let manager = Manager::new().await?;
        let adapter_list = manager.adapters().await?;
        for adapter in adapter_list.into_iter() {
            if let Ok(Some(p)) = Self::get_ecam_from_adapter(&adapter).await {
                let id = p.id();
                return Ok((p.local_name, id));
            }
        }
        Err(EcamError::NotFound)
    }

    /// Searches an adapter for something that meets the definition of [`EcamPeripheral`].
    async fn get_ecam_from_adapter(adapter: &Adapter) -> Result<Option<EcamPeripheral>, EcamError> {
        trace_packet!("Starting scan on {}...", adapter.adapter_info().await?);
        let filter = ScanFilter {
            services: vec![SERVICE_UUID],
        };
        adapter.start_scan(filter).await?;

        for _ in 0..10 {
            time::sleep(Duration::from_millis(500)).await;
            let peripherals = adapter.peripherals().await?;
            for peripheral in peripherals.into_iter() {
                trace_packet!("Found peripheral, address = {:?}", peripheral.address());
                if let Some(peripheral) = EcamPeripheral::validate(peripheral).await? {
                    adapter.stop_scan().await;
                    return Ok(Some(peripheral));
                }
            }
        }

        Ok(None)
    }
}

impl EcamDriver for EcamBT {
    fn read<'a>(&self) -> AsyncFuture<Option<EcamDriverOutput>> {
        Box::pin(self.notifications.recv())
    }

    fn write<'a>(&self, data: EcamDriverPacket) -> AsyncFuture<()> {
        Box::pin(self.peripheral.write(data.packetize()))
    }

    fn alive(&self) -> AsyncFuture<bool> {
        Box::pin(self.peripheral.is_alive())
    }

    fn scan<'a>() -> AsyncFuture<'a, (String, String)>
    where
        Self: Sized,
    {
        Box::pin(Self::scan())
    }
}

/// Holds most of the device BTLE communication functionality.
#[derive(Clone)]
struct EcamPeripheral {
    pub local_name: String,
    peripheral: Peripheral,
    characteristic: Characteristic,
}

impl EcamPeripheral {
    pub async fn write(&self, data: Vec<u8>) -> Result<(), EcamError> {
        trace_packet!("{{host->device}} {}", hexdump(&data));
        Result::Ok(
            self.peripheral
                .write(
                    &self.characteristic,
                    &data,
                    btleplug::api::WriteType::WithResponse,
                )
                .await?,
        )
    }

    pub async fn is_alive(&self) -> Result<bool, EcamError> {
        Ok(self.peripheral.is_connected().await?)
    }

    #[cfg(target_os = "macos")]
    pub fn id(&self) -> String {
        // Icky, but we don't have a PeripheralId to UUID function
        println!("{:?}", self.peripheral.id());
        format!("{:?}", self.peripheral.id())[13..49].to_owned()
    }

    #[cfg(target_os = "linux")]
    pub fn id(&self) -> String {
        self.peripheral.id().to_string()
    }

    pub async fn notifications(&self) -> Result<impl Stream<Item = EcamDriverOutput>, EcamError> {
        trace_packet!("TRYING TO SUBSCRIBE...");
        self.peripheral
            .subscribe(&self.characteristic)
            .await
            .expect("Could not subscribe to characteristic");
        trace_packet!("SUBSCRIBED");
        trace_packet!("Is connected ? {:?}", self.peripheral.is_connected().await);
        let peripheral = self.peripheral.clone();
        let (trigger, tripwire) = Tripwire::new();
        tokio::spawn(async move {
            while peripheral.is_connected().await.unwrap_or_default() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            trace_shutdown!("peripheral.is_connected");
            drop(trigger);
        });

        // Raw stream of bytes from device
        let notifications = self.peripheral.notifications().await?.map(|m| m.value);
        trace_packet!("GOT NOTIFICATIONS stream setup");
        // Parse into packets and stop when device disconnected
        let n = packet_stream(notifications)
            .map(|v| EcamDriverOutput::Packet(EcamDriverPacket::from_slice(unwrap_packet(&v))))
            .take_until_if(tripwire);
        Ok(n)
    }

    /// Assumes that a [`Peripheral`] is a valid ECAM, and connects to it.
    pub async fn connect(peripheral: Peripheral) -> Result<Self, EcamError> {
        peripheral.connect().await?;
        peripheral.discover_services().await?;
        let characteristic = Characteristic {
            uuid: CHARACTERISTIC_UUID,
            service_uuid: SERVICE_UUID,
            properties: CharPropFlags::WRITE | CharPropFlags::READ | CharPropFlags::INDICATE,
        };

        Ok(EcamPeripheral {
            local_name: "unknown".to_owned(),
            peripheral,
            characteristic,
        })
    }

    /// Validates that a [`Peripheral`] is a valid ECAM, and returns `Ok(Some(EcamPeripheral))` if so.
    pub async fn validate(peripheral: Peripheral) -> Result<Option<Self>, EcamError> {
        let properties = peripheral.properties().await?;
        let is_connected = peripheral.is_connected().await?;
        let properties = properties.map_or(Err(EcamError::Unknown), Ok)?;
        if let Some(local_name) = properties.local_name {
            if !is_connected {
                peripheral.connect().await?
            }
            peripheral.is_connected().await?;
            peripheral.discover_services().await?;
            for service in peripheral.services() {
                for characteristic in service.characteristics {
                    if characteristic.uuid == CHARACTERISTIC_UUID {
                        return Ok(Some(EcamPeripheral {
                            local_name,
                            peripheral,
                            characteristic,
                        }));
                    }
                }
            }
            return Ok(None);
        }
        Ok(None)
    }
}
