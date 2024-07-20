use embassy_net::{dns::DnsQueryType, tcp::TcpSocket, IpAddress, Stack};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Receiver};
use embassy_time::{Duration, Instant, Timer};
use embedded_svc::http::{self, client::Request};
use esp32_hal::Rng;
use esp_println::print;
use esp_wifi::wifi::{WifiDevice, WifiStaDevice};
use heapless::Vec;
use log::{info, error};
use esp_backtrace as _;

use crate::soil_estimator;

pub struct DNSAddress<'a> {
    url: &'a str,
    ttl: Duration,
    address: Option<(IpAddress, Instant)>,
    port: u16,
}

impl<'a> DNSAddress<'a> {
    pub fn new(url: &'a str, ttl: Duration, port:u16) -> Self {
        Self { url, address: None, ttl, port}
    }

    pub async fn querry_endpoint(&mut self, stack: &Stack<WifiDevice<'_, WifiStaDevice>>, retry_period: Duration) -> (IpAddress, u16) {
        (self.get_or_querry(stack, retry_period).await, self.port)
    }

    pub fn get_adress(&self) -> Option<IpAddress> {
        self.address.as_ref().and_then(|(address, expery)| (expery > &Instant::now()).then(|| address.clone()))
    }

    pub async fn get_or_querry(&mut self, stack: &Stack<WifiDevice<'_, WifiStaDevice>>, retry_period: Duration) -> IpAddress {
        if let Some(address) = self.get_adress() {
            return address;
        }

        loop {
            match stack.dns_query(self.url, DnsQueryType::A)
            .await
            .map(|mut addresses| addresses.pop()) {
                Ok(Some(address)) => {
                    break self.address.insert((address, Instant::now() + self.ttl)).0;
                },
                Ok(None) => error!("DNS returned no address for {}", self.url),
                Err(err) => error!("DNS querry for {} failed for {:?}.", self.url, err),
            };
            Timer::after(retry_period).await;
        }
    }
}

pub struct UploadDataSource {
    pub messurements: Receiver<'static, NoopRawMutex, soil_estimator::FilteredMessurement, 64>,
}

pub struct UploadData {
    messurements: Vec<soil_estimator::FilteredMessurement, 10>,
}

impl UploadData {
    pub fn new() -> Self {
        Self { messurements: Vec::new() }
    }
    pub async fn ready_to_tx(&mut self, sources: &UploadDataSource) {
        while !self.messurements.is_full() {
            self.messurements.push(sources.messurements.receive().await).unwrap();
        }
    }
}

pub struct Authentication {
    pub local_nonce: u64,
    pub server_noce: u64,
}

pub struct WebClient<const TX_N:usize, const RX_N:usize> {
    tx_buffer: [u8; TX_N],
    rx_buffer: [u8; RX_N],
    rng: Rng,
    auth: Option<Authentication>,
}

impl<const TX_N:usize, const RX_N:usize> WebClient<TX_N, RX_N> {
    pub fn new(rng: Rng) -> Self {
        Self { tx_buffer: [0x0 ; TX_N], rx_buffer: [0x0 ; RX_N], auth: None, rng}
    }


    pub async fn update_server(&mut self, stack: &Stack<WifiDevice<'_, WifiStaDevice>>, dns_address: &mut DNSAddress<'_>, upload_data: &mut UploadData) {
        use embedded_io_async::Write;
        let mut socket = TcpSocket::new(&stack, &mut self.rx_buffer, &mut self.tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));
        let end_point = dns_address.querry_endpoint(stack, Duration::from_secs(60*5)).await;


        if let Err(err) = socket.connect(end_point).await {
            error!("Failed to connect to {} at {:?} for {:?}.", dns_address.url, end_point, err);
            return;
        }
        info!("Connected to {} at {:?}.", dns_address.url, end_point);

        socket.write_all(b"GET /auth HTTP/1.0\r\nHost: ").await;
        socket.write_all(dns_address.url.as_bytes()).await;
        socket.write_all(b"\r\nAuthorization: HANDSHAKE\r\nConnection: keep-alive\r\nUser-Agent: Dewy\r\n\r\n").await;


        let c = http::client::Client::wrap(socket);








        if let Err(err) = socket.write_all(
        b"GET / HTTP/1.0\r\nHost: www.mobile-j.de\r\n\r\n"
        ).await {
            error!("Get to {} failed at {:?} for {:?}.", dns_address.url, end_point, err);
            return;
        }

        upload_data.messurements.clear();

        if let Err(err) = socket.read_with(|rx_buf|{
            if rx_buf.len() == 0 {
                info!("read EOF");
            } else {
                match core::str::from_utf8(rx_buf) {
                    Ok(read_str) => print!("{}", read_str),
                    Err(err) => error!("Could not decode read to utf-8  from {} at {:?} for {:?}.", dns_address.url, end_point, err),
                };
            };
            (rx_buf.len(), ())
        }).await {
            error!("Socket read from {} failed at {:?} for {:?}.", dns_address.url, end_point, err);
        }
    }
}
    // let mut socket = TcpSocket::new(&stack, &mut rx_buffer, &mut tx_buffer);