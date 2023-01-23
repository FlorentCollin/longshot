use crate::ecam::{ecam, EcamStatus};
use crate::operations::{brew, validate_brew, BrewIngredientInfo, IngredientCheckMode};
use crate::protocol::machine_enum::MachineEnumerable;
use crate::protocol::{EcamBeverageId, EcamBeverageTaste};
use crate::{device_common::DeviceCommon, ecam::EcamOutput};
use std::error::Error;
use std::str;
use std::time::Duration;

use rumqttc::{AsyncClient, Event, Key, MqttOptions, TlsConfiguration, Transport};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_stream::StreamExt;

pub struct AwsConfig {
    pub ca: Vec<u8>,
    pub client_cert: Vec<u8>,
    pub client_key: Vec<u8>,
}

pub struct MqttServer {
    pub aws_config: AwsConfig,
    pub client_id: String,
    pub listen_topic: String,
    pub endpoint: String,
}

impl MqttServer {
    pub async fn launch_server(
        self,
        device_common: DeviceCommon,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut mqttoptions = MqttOptions::new(self.client_id, self.endpoint, 8883);
        mqttoptions.set_keep_alive(std::time::Duration::from_secs(10));

        let transport = Transport::Tls(TlsConfiguration::Simple {
            ca: self.aws_config.ca,
            alpn: None,
            client_auth: Some((
                self.aws_config.client_cert,
                Key::RSA(self.aws_config.client_key),
            )),
        });
        mqttoptions.set_transport(transport);
        let topic_prefix = String::from(&self.listen_topic[..self.listen_topic.len() - 1]);
        let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);
        client
            .subscribe(self.listen_topic, rumqttc::QoS::AtLeastOnce)
            .await?;

        // Remove the `+` from the listen_topic
        let _eventloop_task = tokio::task::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(event) => match event {
                        Event::Incoming(packet) => {
                            println!("Received Published event: {:?}", packet);
                            if let rumqttc::Packet::Publish(packet) = packet {
                                if packet.dup {
                                    println!("This event is a duplicated skipping it...");
                                    continue;
                                }

                                if packet.topic.starts_with(&topic_prefix) {
                                    match serde_json::from_slice::<BrewIn>(&packet.payload) {
                                        Err(err) => eprintln!("{:?}", err.to_string()),
                                        Ok(brew_in) => {
                                            println!("{:?}", brew_in);
                                            println!("CALLING THE BREW_MQTT 🎉");
                                            brew_mqtt(&client, brew_in, device_common.clone());
                                        }
                                    }
                                }
                            }
                        }
                        _ => {
                            println!("Event: {:?}", event);
                        }
                    },
                    Err(e) => {
                        eprintln!("Error = {:?}", e);
                        break;
                    }
                }
            }
        })
        .await?;

        Ok(())
    }
}

fn brew_mqtt(client: &AsyncClient, brew_in: BrewIn, device_common: DeviceCommon) {
    let client = client.clone();

    println!("SPAWNING THE BREW_MQTT 🎉");
    tokio::task::spawn(async move {
        println!("RUNNING THE BREW_MQTT 🎉");
        let device_common = device_common.clone();
        let ecam_machine = ecam(&device_common, false)
            .await
            .expect("Could not find the ecam machine");

        let mut tap = ecam_machine
            .packet_tap()
            .await
            .expect("Could not get the `packet_tap` from the ecam machine");

        let beverage: EcamBeverageId =
            EcamBeverageId::lookup_by_name_case_insensitive(&brew_in.drink_order)
                .expect("Invalid beverage");

        let mut ingredients = vec![
            BrewIngredientInfo::Coffee(brew_in.drink_details.coffee),
            BrewIngredientInfo::Taste(
                EcamBeverageTaste::lookup_by_name_case_insensitive(&brew_in.drink_details.taste)
                    .expect("The taste parameter is not valid"),
            ),
        ];
        if let Some(milk) = brew_in.drink_details.milk {
            ingredients.push(BrewIngredientInfo::Milk(milk));
        }

        let recipe = validate_brew(
            ecam_machine.clone(),
            beverage,
            ingredients,
            IngredientCheckMode::AllowDefaults,
        )
        .await
        .expect("The brew recipe is invalid");
        // brew(ecam_machine.clone(), false, beverage, recipe)
        //     .await
        //     .expect("problem during brewing");

        let mut last_status = None;
        while let Some(packet) = tap.next().await {
            match packet {
                EcamOutput::Ready | EcamOutput::Packet(_) => {
                    let status = ecam_machine
                        .current_state()
                        .await
                        .expect("Could not get the current state of the ecam machine");
                    if let Some(last_status) = last_status {
                        if last_status == status {
                            continue;
                        }
                    }
                    last_status = Some(status);
                    let payload = json!(DdbEntry {
                        orderId: brew_in.order_id.clone(),
                        status: status,
                    })
                    .to_string();
                    println!("Got ok status: {payload}");
                    let topic = format!("brew/out/{}", brew_in.order_id);
                    let res = client
                        .publish(topic, rumqttc::QoS::AtLeastOnce, false, payload)
                        .await;
                    if res.is_err() {
                        eprintln!("Error while publishing to MQTT: {:?}", res.unwrap_err());
                    }
                }
                EcamOutput::Done => {
                    println!("Done...");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    break;
                }
            }
        }
    });
}

#[derive(Serialize)]
struct DdbEntry {
    orderId: String,
    #[serde(flatten)]
    status: EcamStatus,
}

#[derive(Deserialize, Debug)]
struct DrinkDetails {
    coffee: u16,
    taste: String,
    milk: Option<u16>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
struct BrewIn {
    user_id: String,
    order_id: String,
    drink_order: String,
    drink_details: DrinkDetails,
}
