#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]



use embassy_executor::Spawner;
use embassy_futures::select;
use embassy_net::{Stack, StackResources};
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp32_hal as hal;
use esp_wifi::{wifi::{WifiController, WifiDevice, WifiEvent, WifiStaDevice, WifiState}, EspWifiInitFor};
use hal::{clock::ClockControl, embassy, gpio::{GpioPin, Output, PushPull}, i2c::I2C, peripherals::{Peripherals, I2C0}, prelude::*, timer::TimerGroup};
use pump_control::PumpController;
use static_cell::make_static;
use embedded_svc::wifi::Wifi;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::{Channel, Sender}, signal::Signal};
use log::{error, info};

mod seesaw;
mod networking;
mod pump_control;
mod soil_estimator;


const SOIL_SENSOR_ADDR: u8 = 0x36;



#[main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger(log::LevelFilter::Info);
    

    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();
    let clocks = make_static!(ClockControl::max(system.clock_control).freeze());
    let io = hal::IO::new(peripherals.GPIO, peripherals.IO_MUX);

    let timer_group0 = TimerGroup::new(peripherals.TIMG0, clocks);
    embassy::init(clocks, timer_group0);

    let timer = hal::timer::TimerGroup::new(peripherals.TIMG1, clocks).timer0;
    let mut rng = hal::Rng::new(peripherals.RNG);

    let wifi_init = esp_wifi::initialize(
        EspWifiInitFor::Wifi,
        timer,
        rng.clone(),
        system.radio_clock_control,
        clocks,
    )
    .unwrap();

    let (wifi_interface, controller) =
        esp_wifi::wifi::new_with_mode(&wifi_init, peripherals.WIFI, WifiStaDevice).unwrap();
    
    let net_config = embassy_net::Config::dhcpv4(Default::default());

    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;

    // Init network stack
    let stack = &*make_static!(Stack::new(
        wifi_interface,
        net_config,
        make_static!(StackResources::<3>::new()),
        seed
    ));

    let i2c = hal::i2c::I2C::new(
        peripherals.I2C0,
        io.pins.gpio18,
        io.pins.gpio19,
        100u32.kHz(),
        clocks,
    );

    let soil_mesurement: &mut Channel::<NoopRawMutex, seesaw::Messurement, 64> = make_static!(Channel::new());
    let messurement_log: &mut Channel::<NoopRawMutex, soil_estimator::FilteredMessurement, 64> = make_static!(Channel::new());
    let pump_target = make_static!(Signal::new());
    let pump_controler = pump_control::PumpController::new(peripherals.MCPWM0, clocks, io.pins.gpio21.into_push_pull_output(), pump_target).unwrap();

    let estimator = soil_estimator::SoilEstimator::new(soil_mesurement.receiver(), pump_target, messurement_log.sender());

    let upload_sources = networking::UploadDataSource {
        messurements: messurement_log.receiver(),
    };

    spawner.spawn(connection_task(controller)).unwrap();
    spawner.spawn(net_task(&stack)).unwrap();
    spawner.spawn(net_app_task(&stack, upload_sources, rng)).unwrap();
    spawner.spawn(i2c_task(i2c, soil_mesurement.sender())).unwrap();
    spawner.spawn(pump_task(pump_controler)).unwrap();
    spawner.spawn(estimator_task(estimator)).unwrap();
    
    loop {
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[embassy_executor::task]
async fn estimator_task(mut estimator: soil_estimator::SoilEstimator<'static, 64, 64>) {
    loop {
        estimator.update_estimator().await;
    }
}

#[embassy_executor::task]
async fn pump_task(controler: PumpController<'static, GpioPin<Output<PushPull>, 21>>) {
    controler.run_motor_control().await;
}

#[embassy_executor::task]
async fn i2c_task(i2c: I2C<'static, I2C0>, soil_messurement: Sender<'static, NoopRawMutex, seesaw::Messurement, 64>) {
    let mut i2c_interface = seesaw::I2CInterfaces::new(i2c);
    let mut soil_sensor = seesaw::SoilSensor::new(SOIL_SENSOR_ADDR, soil_messurement);
    let mut run_at = Instant::now();
    loop {
        soil_sensor.run(&mut i2c_interface).await;
        run_at += Duration::from_secs(2);
        Timer::at(run_at).await;
    }
}

#[embassy_executor::task]
async fn connection_task(mut controller: WifiController<'static>) {
    info!("Wifi Device capabilities: {:?}", controller.get_capabilities());
    loop {
        if let WifiState::StaConnected =  esp_wifi::wifi::get_wifi_state() {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = embedded_svc::wifi::Configuration::Client(embedded_svc::wifi::ClientConfiguration {
                ssid: SSID.try_into().unwrap(),
                password: PASSWORD.try_into().unwrap(),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            controller.start().await.unwrap();
            info!("Wifi started!");
        }

        match controller.connect().await {
            Ok(_) => info!("Wifi connected!"),
            Err(e) => {
                error!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await;
}

const URL: &str = &"www.mobile-j.de";
const DNS_TTL: Duration = Duration::from_secs(60 * 60);
const PORT: u16 = 80;

#[embassy_executor::task]
async fn net_app_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>, upload: networking::UploadDataSource, rng: hal::Rng) {

    let mut dns_address = networking::DNSAddress::new(URL, DNS_TTL, PORT);
    let mut upload_data = networking::UploadData::new();
    let mut client = networking::WebClient::<4096, 4096>::new(rng);

    loop {
        stack.wait_config_up().await;
        select::select(upload_data.ready_to_tx(&upload), Timer::after(Duration::from_secs(60*5))).await;
        client.update_server(&stack, &mut dns_address, &mut upload_data).await;
    }
}

