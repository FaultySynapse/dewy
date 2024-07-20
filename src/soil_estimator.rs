use core::ops::Rem;

use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::{Receiver, Sender}, signal::Signal};
use crate::seesaw;
use log::{info, error};

#[derive(Debug, Clone, Copy, Default)]
pub struct FilteredMessurement {
    pub moisture: f64,
    pub temperature: f64,
}

pub struct SoilEstimator<'a, const RN: usize, const ON: usize>{
    messurements: Receiver<'a, NoopRawMutex, seesaw::Messurement, RN>,
    messurement_log: Sender<'a, NoopRawMutex, FilteredMessurement, ON>,
    command: &'a Signal<NoopRawMutex, u8>,
    low_pass_messurement: FilteredMessurement,
    samples: u64,
}

impl<'a, const RN: usize, const ON: usize> SoilEstimator<'a, RN, ON> {
    pub fn new(messurements: Receiver<'a, NoopRawMutex, seesaw::Messurement, RN>, command: &'a Signal<NoopRawMutex, u8>, filtered: Sender<'a, NoopRawMutex, FilteredMessurement, ON>) -> Self {
        Self {
            messurements, command, low_pass_messurement: FilteredMessurement::default(), samples: 0, messurement_log: filtered
        }
    }

    pub async fn update_estimator(&mut self) {
        let sample = self.messurements.receive().await;
        self.samples += 1;
        self.low_pass_messurement.moisture    = 0.5 * self.low_pass_messurement.moisture    + 0.5 * sample.moisture as f64;
        self.low_pass_messurement.temperature = 0.5 * self.low_pass_messurement.temperature + 0.5 * sample.temp as f64;
        info!("Estimator state: {:?}", self.low_pass_messurement);
        if self.samples > 50 {
            if self.samples.rem(30) == 0 {
                if let Err(err) = self.messurement_log.try_send(self.low_pass_messurement) {
                    error!("Failed to log {:?}", err);
                }
            }
            self.command.signal(0);
        }
    }
}