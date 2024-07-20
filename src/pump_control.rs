
use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use esp32_hal::{clock::Clocks, gpio::OutputPin, mcpwm::{operator::PwmPinConfig, timer::{PwmWorkingMode, TimerClockConfig}, FrequencyError, PeripheralClockConfig, MCPWM}, peripheral::Peripheral, peripherals::MCPWM0, prelude::_fugit_RateExtU32};
pub struct PumpController<'a, P: Peripheral> {
    peripheral: MCPWM0,
    pwm_config: PeripheralClockConfig<'a>,
    timer_config: TimerClockConfig<'a>,
    pin: P,
    target: &'a Signal<NoopRawMutex, u8>,
}

impl<'a, P:Peripheral + 'a> PumpController<'a, P>
where <P as Peripheral>::P: OutputPin
{
    pub fn new<'b: 'a>(peripheral: MCPWM0, clocks: &'b Clocks<'a>, pin: P, target: &'a Signal<NoopRawMutex, u8>) -> Result<Self, FrequencyError> {
        PeripheralClockConfig::with_frequency(clocks, 5400u32.kHz()).and_then(|pwm_config|{
            pwm_config.timer_clock_with_frequency(256, PwmWorkingMode::Increase, 20u32.kHz()).map(move |timer_config| {
                Self{
                    peripheral,
                    pwm_config,
                    timer_config,
                    pin,
                    target,
                }
            })
        })
    }

    pub async fn run_motor_control(self)
    {
        let mut mcpwm = MCPWM::new(self.peripheral, self.pwm_config);
        mcpwm.operator0.set_timer(&mcpwm.timer0);
        let mut pwm_pin = mcpwm
        .operator0
        .with_pin_a(self.pin, PwmPinConfig::UP_ACTIVE_HIGH);
        loop {
            let mut target = self.target.wait().await;
            if target < 1 {
                let timeout = Instant::now() + Duration::from_secs(2);
                loop {
                    match select(self.target.wait(), Timer::at(timeout)).await{
                        embassy_futures::select::Either::First(new_target) => {
                            target = new_target;
                            if target > 1 {
                                break;
                            }
                        },
                        embassy_futures::select::Either::Second(_) => {
                            mcpwm.timer0.stop();
                            while target < 1 {
                                target = self.target.wait().await;
                            }
                            mcpwm.timer0.start(self.timer_config);
                            break;
                        },
                    }
                }
            }
            pwm_pin.set_timestamp(target as u16);
        }
    }
}