//! Low-level communication with ECAM-based devices.

use crate::prelude::*;

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
