use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;
use embassy_sync::{blocking_mutex::raw::RawMutex, channel::Sender};
use log::{info, warn, error};
use esp_backtrace as _;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum SeesawReg {
    Status(SeesawStatus),
    GPIO,
    Sercom0,

    Timer,
    Adc,
    Dac,
    Interrupt,
    Dap,
    Eeprom,
    Neopixel,
    Touch(SeesawTouch),
    Keypad,
    Encoder,
    Spectrum,
}

impl SeesawReg {
    fn in_options(&self, options: u32) -> bool {
        let [base, _] = self.get_register();
        options & (1u32 << base) != 0
    }
    fn get_register(&self) -> [u8; 2] {
        match self {
            Self::Status(status) => [0x00, status.clone() as u8],
            Self::GPIO => [0x01, 0x00],
            Self::Sercom0 => [0x02, 0x00],

            Self::Timer => [0x08, 0x00],
            Self::Adc => [0x09, 0x00],
            Self::Dac => [0x0A, 0x00],
            Self::Interrupt => [0x0B, 0x00],
            Self::Dap => [0x0C, 0x00],
            Self::Eeprom => [0x0D, 0x00],
            Self::Neopixel => [0x0E, 0x00],
            Self::Touch(touch) => [0x0F, touch.clone() as u8],
            Self::Keypad => [0x10, 0x00],
            Self::Encoder => [0x11, 0x00],
            Self::Spectrum => [0x12, 0x00],
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SeesawStatus {
    HwId = 0x01,
    Version = 0x02,
    Options = 0x03,
    TEMP = 0x04,
    Reset = 0x7F,
}

#[derive(Debug, Clone, Copy)]
enum SeesawTouch {
    ChannelOffset = 0x10,
}

pub struct I2CInterfaces<T> {
    i2c: T
}

impl<T:I2c> I2CInterfaces<T> {
    pub fn new(i2c: T) -> Self {
        Self { i2c }
    }

    async fn seesaw_request(&mut self, address: u8, reg: &SeesawReg) -> Result<(), T::Error>{
        self.i2c.write(address, &reg.get_register()).await
    }

    async fn seesaw_read_u8(&mut self, address: u8) -> Result<u8, T::Error>{
        let mut read_buf = [0x00 ; 1];
        match self.i2c.read(address, &mut read_buf).await {
            Ok(_) => Ok(u8::from_be_bytes(read_buf)),
            Err(err) => Err(err),
        }
    }

    async fn seesaw_read_u16(&mut self, address: u8) -> Result<u16, T::Error>{
        let mut read_buf = [0x00 ; 2];
        match self.i2c.read(address, &mut read_buf).await {
            Ok(_) => Ok(u16::from_be_bytes(read_buf)),
            Err(err) => Err(err),
        }
    }

    async fn seesaw_read_u32(&mut self, address: u8) -> Result<u32, T::Error>{
        let mut read_buf = [0x00 ; 4];
        match self.i2c.read(address, &mut read_buf).await {
            Ok(_) => Ok(u32::from_be_bytes(read_buf)),
            Err(err) => Err(err),
        }
    }
}

enum SoilSensorState {
    Init,
    Messuring,
    Error,
}

#[derive(Debug)]
pub struct Messurement {
    pub temp: f32,
    pub moisture: u16,
}

pub struct SoilSensor<'a, M:RawMutex, const N: usize> {
    address: u8,
    state: SoilSensorState,
    sender: Sender<'a, M, Messurement, N>,
}

impl<'a, M: RawMutex, const N: usize> SoilSensor<'a, M, N> {
    pub fn new(address:u8, sender: Sender<'a, M, Messurement, N>) -> Self {
        Self {
            address,
            state: SoilSensorState::Init,
            sender,
        }
    }
    pub async fn run<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>) {
        self.state = match self.state {
            SoilSensorState::Init => {
               match self.init(i2c).await {
                Ok(state) => state,
                Err(err) => {
                    error!("Soil sensor init failed for {:?}", err);
                    SoilSensorState::Error
                },
               } 
            },
            SoilSensorState::Messuring => {
                match self.take_messurement(i2c).await{
                    Ok(messurement) => {
                        info!("Soil messurement {:?}", messurement);
                        self.sender.send(messurement).await;
                        SoilSensorState::Messuring
                    }
                    Err(err) => {
                        error!("Failed to to take messurement {:?}", err);
                        SoilSensorState::Error
                    },
                }
            },
            SoilSensorState::Error => {
                match self.reset_sensor(i2c).await{
                    Ok(_) => SoilSensorState::Init,
                    Err(err) => {
                        warn!("Soil sensor reset failed {:?}", err);
                        SoilSensorState::Error
                    }
                }
            },
        }
    }
    async fn init<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>)-> Result<SoilSensorState, I::Error> {
        info!("Soil sensor Seesaw HW ID: {}", self.read_hw_id(i2c).await?);
        info!("Soil sensor Seesaw Version: {}", self.read_version(i2c).await?);
        if self.check_options(i2c).await? {
            info!("Soil sensor Seesaw options ok");
            Ok(SoilSensorState::Messuring)
        } else {
            error!("Soil sensor Seesaw does not have needed options!");
            Ok(SoilSensorState::Error)
        }
    }
    async fn take_messurement<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>)-> Result<Messurement, I::Error> {
        Ok(Messurement{
            temp: self.read_temp(i2c).await?,
            moisture: self.read_moisture(i2c).await?,
        })
    }
    async fn reset_sensor<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>)-> Result<(), I::Error> {
        i2c.seesaw_request(self.address, &SeesawReg::Status(SeesawStatus::Reset)).await
    }
    async fn read_hw_id<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>)-> Result<u8, I::Error> {
        i2c.seesaw_request(self.address, &SeesawReg::Status(SeesawStatus::HwId)).await?;
        i2c.seesaw_read_u8(self.address).await
    }
    async fn read_version<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>)-> Result<u16, I::Error> {
        i2c.seesaw_request(self.address, &SeesawReg::Status(SeesawStatus::Version)).await?;
        i2c.seesaw_read_u16(self.address).await
    }
    async fn check_options<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>)-> Result<bool, I::Error> {
        i2c.seesaw_request(self.address, &SeesawReg::Status(SeesawStatus::Options)).await?;
        let options = i2c.seesaw_read_u32(self.address).await?;
        info!("Soil sensor opions {:b}", options);
        Ok(SeesawReg::Touch(SeesawTouch::ChannelOffset).in_options(options))
    }
    async fn read_temp<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>)-> Result<f32, I::Error> {
        i2c.seesaw_request(self.address, &SeesawReg::Status(SeesawStatus::TEMP)).await?;
        Ok((1.0 / (1u32 << 16) as f32) * i2c.seesaw_read_u32(self.address).await? as f32)
    }
    async fn read_moisture<I: I2c>(&mut self, i2c: &mut I2CInterfaces<I>)-> Result<u16, I::Error> {
        i2c.seesaw_request(self.address, &SeesawReg::Touch(SeesawTouch::ChannelOffset)).await?;
        Timer::after(Duration::from_millis(3000)).await;
        i2c.seesaw_read_u16(self.address).await
    }
}