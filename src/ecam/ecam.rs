use crate::packet::EcamPacket;
use crate::prelude::*;

use tokio::sync::Mutex;
use tokio_stream::wrappers::BroadcastStream;

use crate::command::*;
use crate::ecam::{hardware_enums::EcamMachineState, EcamDriver, EcamError, EcamOutput};

#[derive(Debug, PartialEq)]
pub enum EcamStatus {
    Unknown,
    StandBy,
    Ready,
    Busy,
}

impl EcamStatus {
    fn extract(state: &MonitorState) -> EcamStatus {
        if state.state == EcamMachineState::StandBy {
            return EcamStatus::StandBy;
        }
        if state.state == EcamMachineState::ReadyOrDispensing && state.progress == 0 {
            return EcamStatus::Ready;
        }
        EcamStatus::Busy
    }

    fn matches(&self, state: &MonitorState) -> bool {
        *self == Self::extract(state)
    }
}

struct StatusInterest {
    count: Arc<std::sync::Mutex<usize>>,
}

struct StatusInterestHandle {
    count: Arc<std::sync::Mutex<usize>>,
}

impl StatusInterest {
    fn new() -> Self {
        StatusInterest {
            count: Arc::new(std::sync::Mutex::new(0)),
        }
    }

    fn lock(&mut self) -> StatusInterestHandle {
        *self.count.lock().unwrap() += 1;
        StatusInterestHandle {
            count: self.count.clone(),
        }
    }

    fn count(&self) -> usize {
        *self.count.lock().unwrap()
    }
}

impl Drop for StatusInterestHandle {
    fn drop(&mut self) {
        *self.count.lock().unwrap() -= 1;
    }
}

#[derive(Clone)]
pub struct Ecam {
    driver: Arc<Box<dyn EcamDriver>>,
    internals: Arc<Mutex<EcamInternals>>,
    alive: Arc<std::sync::Mutex<bool>>,
}

struct EcamInternals {
    last_status: tokio::sync::watch::Receiver<Option<MonitorState>>,
    packet_tap: Arc<tokio::sync::broadcast::Sender<EcamOutput>>,
    ready_lock: Arc<tokio::sync::Semaphore>,
    status_interest: StatusInterest,
}

impl Ecam {
    pub async fn new(driver: Box<dyn EcamDriver>) -> Self {
        let driver = Arc::new(driver);
        let (tx, rx) = tokio::sync::watch::channel(None);
        let (txb, _) = tokio::sync::broadcast::channel(100);

        // We want to lock the status until we've received at least one packet
        let ready_lock = Arc::new(tokio::sync::Semaphore::new(1));
        let mut ready_lock_semaphore = Some(
            ready_lock
                .clone()
                .acquire_owned()
                .await
                .expect("Failed to lock mutex"),
        );

        let internals = Arc::new(Mutex::new(EcamInternals {
            last_status: rx,
            packet_tap: Arc::new(txb),
            ready_lock,
            status_interest: StatusInterest::new(),
        }));
        let ecam_result = Ecam {
            driver,
            internals,
            alive: Arc::new(true.into()),
        };

        let ecam = ecam_result.clone();
        tokio::spawn(async move {
            let packet_tap_sender = ecam.internals.lock().await.packet_tap.clone();
            let mut started = false;
            while ecam.is_alive() {
                // Treat end-of-stream as EcamOutput::Done, but we might want to reconsider this in the future
                let packet = ecam.driver.read().await?.unwrap_or(EcamOutput::Done);
                let _ = packet_tap_sender.send(packet.clone());
                match packet {
                    EcamOutput::Ready => {
                        if started {
                            warning!("Got multiple start requests");
                        } else {
                            tokio::spawn(ecam.clone().write_monitor_loop());
                            started = true;
                        }
                    }
                    EcamOutput::Done => {
                        break;
                    }
                    EcamOutput::Packet(EcamPacket {
                        representation: Response::State(x),
                        ..
                    }) => {
                        if tx.send(Some(x)).is_err() {
                            break;
                        }
                        ready_lock_semaphore.take();
                    }
                    _ => {}
                }
            }
            println!("Closed");
            ecam.deaden();
            Result::<(), EcamError>::Ok(())
        });

        ecam_result
    }

    /// Blocks until the device state reaches our desired state.
    pub async fn wait_for_state(&self, state: EcamStatus) -> Result<(), EcamError> {
        let mut internals = self.internals.lock().await;
        let mut rx = internals.last_status.clone();
        let status_interest = internals.status_interest.lock();
        drop(internals);
        loop {
            if let Some(test) = rx.borrow().as_ref() {
                if state.matches(test) {
                    drop(status_interest);
                    return Ok(());
                }
            }
            // TODO: timeout
            rx.changed().await.map_err(|_| EcamError::Unknown)?;
        }
    }

    /// Returns the current state, or blocks if we don't know what the current state is yet.
    pub async fn current_state(&self) -> Result<EcamStatus, EcamError> {
        let mut internals = self.internals.lock().await;
        let status_interest = internals.status_interest.lock();
        let rx = internals.last_status.clone();
        let ready_lock = internals.ready_lock.clone();
        drop(internals);
        drop(
            ready_lock
                .acquire_owned()
                .await
                .map_err(|_| EcamError::Unknown)?,
        );
        let ret = if let Some(test) = rx.borrow().as_ref() {
            Ok(EcamStatus::extract(test))
        } else {
            Err(EcamError::Unknown)
        };
        drop(status_interest);
        ret
    }

    pub async fn write(&self, packet: EcamPacket<Request>) -> Result<(), EcamError> {
        self.driver.write(packet.encode()).await
    }

    pub async fn packet_tap(&self) -> Result<impl Stream<Item = EcamOutput>, EcamError> {
        let internals = self.internals.lock().await;
        Ok(BroadcastStream::new(internals.packet_tap.subscribe())
            .map(|x| x.expect("Unexpected receive error")))
    }

    pub fn is_alive(&self) -> bool {
        if let Ok(alive) = self.alive.lock() {
            *alive
        } else {
            false
        }
    }

    /// The monitor loop is booted when the underlying driver reports that it is ready.
    async fn write_monitor_loop(self) -> Result<(), EcamError> {
        let status_request = Request::Monitor(MonitorRequestVersion::V2).encode();
        while self.is_alive() {
            // Only send status update packets while there is status interest
            if self.internals.lock().await.status_interest.count() == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            match tokio::time::timeout(
                Duration::from_millis(250),
                self.driver.write(status_request.clone()),
            )
            .await
            {
                Ok(Err(_)) => {
                    warning!("Failed to request status");
                }
                Err(_) => {
                    warning!("Status request send timeout");
                }
                _ => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
        warning!("Sending loop died.");
        self.deaden();
        Ok(())
    }

    fn deaden(&self) {
        if let Ok(mut alive) = self.alive.lock() {
            *alive = false;
        }
    }
}

impl Drop for Ecam {
    fn drop(&mut self) {
        self.deaden()
    }
}
