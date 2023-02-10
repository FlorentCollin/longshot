use crate::ecam::{get_ecam_simulator, Ecam, EcamBT, EcamStatus};
use crate::operations::{brew, validate_brew, BrewIngredientInfo, IngredientCheckMode};
use crate::protocol::machine_enum::MachineEnumerable;
use crate::protocol::{EcamBeverageId, EcamBeverageTaste};
use crate::{device_common::DeviceCommon, ecam::EcamOutput};
use std::str;
use std::time::Duration;

use rumqttc::{AsyncClient, Event, Key, MqttOptions, TlsConfiguration, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::StreamExt;

pub struct AwsConfig {
    pub ca: Vec<u8>,
    pub client_cert: Vec<u8>,
    pub client_key: Vec<u8>,
}

pub struct MqttServer {
    pub aws_config: AwsConfig,
    pub client_id: String,
    pub topic_in: String,
    pub topic_out: String,
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
        // Remove the `+` from the listen_topic
        let topic_prefix = String::from(&self.topic_in[..self.topic_in.len() - 1]);
        let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);
        client
            .subscribe(self.topic_in, rumqttc::QoS::AtLeastOnce)
            .await?;

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
                                    println!(
                                        "{:?}",
                                        serde_json::from_slice::<Value>(&packet.payload)
                                    );
                                    match serde_json::from_slice::<BrewIn>(&packet.payload) {
                                        Err(err) => eprintln!("{:?}", err.to_string()),
                                        Ok(brew_in) => {
                                            println!("{:?}", brew_in);
                                            println!("CALLING THE BREW_MQTT 🎉");
                                            brew_mqtt(
                                                &client,
                                                brew_in,
                                                self.topic_out.clone(),
                                                device_common.clone(),
                                            );
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

fn brew_mqtt(client: &AsyncClient, brew_in: BrewIn, topic: String, device_common: DeviceCommon) {
    let client = client.clone();

    println!("SPAWNING THE BREW_MQTT 🎉");
    tokio::task::spawn(async move {
        println!("RUNNING THE BREW_MQTT 🎉");
        let device_common = device_common.clone();
        let device_name = device_common.device_name;
        let ecam_machine = if device_name.starts_with("sim") {
            Ecam::new(
                Box::new(
                    get_ecam_simulator(&device_name)
                        .await
                        .expect("Could not get simulator"),
                ),
                false,
            )
            .await
        } else {
            Ecam::new(
                Box::new(
                    EcamBT::get(device_name)
                        .await
                        .expect("Could not get bluetooth simulator"),
                ),
                false,
            )
            .await
        };
        // .await.expect("Could not find the ecam machine");

        let mut tap = ecam_machine
            .packet_tap()
            .await
            .expect("Could not get the `packet_tap` from the ecam machine");

        let beverage: EcamBeverageId =
            EcamBeverageId::lookup_by_name_case_insensitive(&brew_in.drink_order)
                .expect("Invalid beverage");

        // Setup the ingredients
        let mut ingredients = vec![];
        if let Some(coffee) = brew_in.drink_details.coffee {
            ingredients.push(BrewIngredientInfo::Coffee(coffee));
        }
        if let Some(taste) = brew_in.drink_details.taste {
            ingredients.push(BrewIngredientInfo::Taste(
                EcamBeverageTaste::lookup_by_name_case_insensitive(&taste)
                    .expect("The taste parameter is not valid"),
            ));
        }
        if let Some(milk) = brew_in.drink_details.milk {
            ingredients.push(BrewIngredientInfo::Milk(milk));
        }
        if let Some(hotwater) = brew_in.drink_details.hotwater {
            ingredients.push(BrewIngredientInfo::HotWater(hotwater));
        }

        let recipe = validate_brew(
            ecam_machine.clone(),
            beverage,
            ingredients,
            IngredientCheckMode::AllowDefaults,
        )
        .await
        .expect("The brew recipe is invalid");

        let ecam_machine_brew = ecam_machine.clone();
        let brew_task = tokio::task::spawn(async move {
            brew(ecam_machine_brew, false, beverage, recipe)
                .await
                .expect("Error while brewing");
        });

        let mut last_status = None;
        let topic = format!("{}/{}", topic, brew_in.order_id);

        // Send a first notifcation so the frontend know the order is in processing
        let payload = json!(DdbEntry {
            user_id: brew_in.user_id.clone(),
            order_id: brew_in.order_id.clone(),
            status: EcamStatus::Ready,
        })
        .to_string();
        let _ = client
            .publish(&topic, rumqttc::QoS::AtLeastOnce, false, payload)
            .await;
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
                        user_id: brew_in.user_id.clone(),
                        order_id: brew_in.order_id.clone(),
                        status: status,
                    })
                    .to_string();
                    println!("Got ok status: {payload}");

                    let res = client
                        .publish(&topic, rumqttc::QoS::AtLeastOnce, false, payload)
                        .await;
                    if res.is_err() {
                        eprintln!("Error while publishing to MQTT: {:?}", res.unwrap_err());
                    }
                }
                EcamOutput::Done => {
                    println!("Done...");
                    let payload = json!(DdbEntry {
                        user_id: brew_in.user_id.clone(),
                        order_id: brew_in.order_id.clone(),
                        status: EcamStatus::Completed,
                    })
                    .to_string();

                    let _ = client
                        .publish(&topic, rumqttc::QoS::AtLeastOnce, false, payload)
                        .await;
                    // Hack
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let _ = ecam_machine.send_done().await;
                    break;
                }
            }
        }
        brew_task.await.expect("Error during the brew task");
    });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DdbEntry {
    user_id: String,
    order_id: String,
    #[serde(flatten)]
    status: EcamStatus,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
struct DrinkDetails {
    coffee: Option<u16>,
    taste: Option<String>,
    milk: Option<u16>,
    hotwater: Option<u16>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
struct BrewIn {
    user_id: String,
    order_id: String,
    drink_order: String,
    drink_details: DrinkDetails,
}
