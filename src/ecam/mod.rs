//! Low-level communication with ECAM-based devices.

use crate::{device_common::DeviceCommon, display, operations::power_on, prelude::*};

use thiserror::Error;

mod driver;
mod ecam_bt;
mod ecam_simulate;
mod ecam_subprocess;
mod ecam_wrapper;
mod packet_receiver;
mod packet_stream;
mod stdin_stream;

pub use self::ecam_bt::EcamBT;
pub use driver::{EcamDriver, EcamDriverOutput};
pub use ecam_simulate::get_ecam_simulator;
pub use ecam_subprocess::connect as get_ecam_subprocess;
pub use ecam_wrapper::{Ecam, EcamOutput, EcamStatus};
pub use packet_receiver::EcamPacketReceiver;
pub use stdin_stream::pipe_stdin;

pub async fn ecam_scan() -> Result<(String, String), EcamError> {
    EcamBT::scan().await
}

pub async fn ecam_lookup(device_name: &str, dump_packets: bool) -> Result<Ecam, EcamError> {
    let driver = Box::new(get_ecam_subprocess(device_name).await?);
    Ok(Ecam::new(driver, dump_packets).await)
}

pub async fn ecam(
    device_common: &DeviceCommon,
    allow_off_and_alarms: bool,
) -> Result<Ecam, EcamError> {
    let ecam = ecam_lookup(&device_common.device_name, device_common.dump_packets).await?;
    if !power_on(
        ecam.clone(),
        device_common.allow_off | allow_off_and_alarms,
        allow_off_and_alarms,
        device_common.turn_on,
    )
    .await?
    {
        display::shutdown();
        std::process::exit(1);
    }
    Ok(ecam)
}

#[derive(Error, Debug)]
pub enum EcamError {
    #[error("not found")]
    NotFound,
    #[error(transparent)]
    BTError(#[from] btleplug::Error),
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    #[error("Unknown error")]
    Unknown,
}
