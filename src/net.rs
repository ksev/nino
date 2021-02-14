use std::convert::{TryFrom, TryInto};
use std::sync::Arc;

use anyhow::Result;
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;

use crate::{
    sensor::{SensorId, SensorMessage, Sensors},
    Config, Global, VERSION,
};

mod proto {
    include!(concat!(env!("OUT_DIR"), "/nino.net.rs"));
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum MessageId {
    Hello = 0,
    Ready = 1,
    Value = 2,
    Sensors = 3,
    SensorConfig = 4,
    AddSensor = 5,
    Pwm = 6,
}

impl TryFrom<u16> for MessageId {
    type Error = anyhow::Error;

    fn try_from(value: u16) -> Result<Self, anyhow::Error> {
        Ok(match value {
            0 => MessageId::Hello,
            1 => MessageId::Ready,
            2 => MessageId::Value,
            3 => MessageId::Sensors,
            4 => MessageId::SensorConfig,
            5 => MessageId::AddSensor,
            6 => MessageId::Pwm,
            _ => anyhow::bail!("{} does not match MessageId", value),
        })
    }
}

pub async fn handle(
    mut socket: TcpStream,
    broadcast: Arc<tokio::sync::broadcast::Sender<SensorMessage>>,
    pwm: crossbeam_channel::Sender<(crate::PwmChannel, f32)>,
) -> Result<()> {
    let (rdr, wrt) = socket.split();

    let mut rdr = BufReader::new(rdr);
    let mut wrt = BufWriter::new(wrt);

    // Say hello to the client
    send_hello(&mut wrt).await?;

    // Expect the client to respond with a ready
    let (rdy, _) = receive_package(&mut rdr).await?;

    if rdy != MessageId::Ready {
        anyhow::bail!("Client did not respond with ready");
    }

    send_sensors(&mut wrt).await?;

    let mut updates = broadcast.subscribe();

    loop {
        tokio::select! {
            Ok(data) = updates.recv() => {
                use SensorMessage::*;
                match data {
                    Update(id, value) => send_value(id, value, &mut wrt).await?,
                    Config(_) | Error(_) | ClearError(_) => send_sensors(&mut wrt).await?,
                    _ => {}
                }
            },
            rdy = receive_package(&mut rdr) => {
                match rdy {
                    Ok((id, buffer)) => handle_package(id, buffer, &pwm).await?,
                    Err(e) => log::error!("Recv error {:?}", e),
                }
            }
        }
    }
}

async fn handle_package(
    id: MessageId,
    data: Vec<u8>,
    pwm: &crossbeam_channel::Sender<(crate::PwmChannel, f32)>,
) -> Result<()> {
    let sensors = Sensors::global();

    match id {
        MessageId::SensorConfig => {
            let cfg = proto::SensorConfig::decode(data.as_slice())?;
            let id = SensorId::from_usize(cfg.id as usize);

            let rate = cfg
                .optional_rate
                .map(|proto::sensor_config::OptionalRate::Rate(r)| r as usize);
            let source = cfg
                .optional_source
                .map(|proto::sensor_config::OptionalSource::Source(s)| s.into());

            sensors.reconfigure(&id, cfg.alias, cfg.unit, rate, source);
        }
        MessageId::AddSensor => {
            sensors.add_virtual();
        }
        MessageId::Pwm => {
            let p = proto::SetPwm::decode(data.as_slice())?;

            let chan = match p.channel {
                0 => crate::PwmChannel::Pwm0,
                1 => crate::PwmChannel::Pwm1,
                _ => return Ok(()),
            };

            if let Err(e) = pwm.try_send((chan, p.value)) {
                log::error!("Could not send to PWM\n{:?}", e);
            }
        }
        _ => { /* Simply ignore the rest, we dont deal with them here */ }
    }

    Ok(())
}

async fn send_hello<T>(socket: &mut T) -> Result<()>
where
    T: AsyncWrite + Unpin,
{
    let cfg = Config::global();
    let database = sled::Db::global();

    let pwm0 = database.get("pwm0").ok().flatten().and_then(|v| {
        let value: &[u8] = &v;
        let value = f32::from_be_bytes(value.try_into().ok()?);
        Some(value)
    }).unwrap_or(0.6);

    let pwm1 = database.get("pwm1").ok().flatten().and_then(|v| {
        let value: &[u8] = &v;
        let value = f32::from_be_bytes(value.try_into().ok()?);
        Some(value)
    }).unwrap_or(0.28);

    let hello = proto::Hello {
        version: VERSION.into(),
        name: cfg.name.clone(),
        retention: cfg.retention as u32,
        pwm0,
        pwm1,
    };

    send_package(socket, MessageId::Hello, hello).await?;

    Ok(())
}

async fn send_value<T>(id: SensorId, value: f64, socket: &mut T) -> Result<()>
where
    T: AsyncWrite + Unpin,
{
    let value = proto::Value {
        id: id.to_usize() as u32,
        value,
    };

    send_package(socket, MessageId::Value, value).await?;

    Ok(())
}

async fn send_sensors<T>(socket: &mut T) -> Result<()>
where
    T: AsyncWrite + Unpin,
{
    let sensors = Sensors::global();

    let data = sensors
        .iter()
        .map(|o| proto::sensors::Sensor {
            id: o.key().to_usize() as u32,
            rate: o.rate as u32,
            alias: (&o.alias).into(),
            unit: (&o.unit).into(),
            values: o.values.iter().map(|v| *v).collect(),
            optional_source: o
                .source
                .as_ref()
                .map(|s| proto::sensors::sensor::OptionalSource::Source(s.into())),
            optional_error: o
                .error
                .as_ref()
                .map(|e| proto::sensors::sensor::OptionalError::Error(e.into())),
        })
        .collect();

    let value = proto::Sensors { sensors: data };

    send_package(socket, MessageId::Sensors, value).await?;

    Ok(())
}

async fn receive_package<T>(socket: &mut T) -> Result<(MessageId, Vec<u8>)>
where
    T: AsyncRead + Unpin,
{
    let message_id = MessageId::try_from(socket.read_u16_le().await?)?;
    let data_len = (socket.read_u64_le().await?) as usize;

    if data_len > 1024 * 1024 * 10 {
        // Dont accept a payload over 10 mega bytes
        anyhow::bail!("Recv data_lengt exceeds maximum {}", data_len);
    }

    let mut out = vec![0; data_len];
    socket.read_exact(&mut out).await?;

    Ok((message_id, out))
}

async fn send_package<T, P>(socket: &mut T, id: MessageId, package: P) -> Result<()>
where
    T: AsyncWrite + Unpin,
    P: prost::Message,
{
    let mut buf = Vec::with_capacity(package.encoded_len());
    package.encode(&mut buf)?;

    // Write the message id first
    socket.write_u16_le(id as u16).await?;

    // Write the length of the data then the data
    socket.write_u64_le(buf.len() as u64).await?;

    socket.write_all(&mut buf).await?;
    socket.flush().await?;

    Ok(())
}
