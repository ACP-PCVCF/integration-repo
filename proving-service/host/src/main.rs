// These constants represent the RISC-V ELF and the image ID generated by risc0-build.
// The ELF is used for proving and the ID is used for verification.
use methods::{ GUEST_PROOFING_LOGIC_ELF, GUEST_PROOFING_LOGIC_ID };
use risc0_zkvm::{ default_prover, ExecutorEnv, Receipt, ReceiptKind };
use serde::{ Deserialize, Serialize };
use rdkafka::consumer::{ Consumer, StreamConsumer };
use rdkafka::producer::{ FutureProducer, FutureRecord };
use rdkafka::config::ClientConfig;
use tokio::time::Duration;
use rdkafka::message::Message;
use serde_json::json;
use log::info;
use serde_json::Value;
use ::std::collections::HashMap;
use proving_service_core::proofing_document::*;

use base64::{ engine::general_purpose, Engine as _ };

use axum::{ routing::get, routing::post, Router, Json };

#[derive(Serialize)]
#[allow(non_snake_case)]
struct ProofResponse {
    productFootprintId: String,
    proofReceipt: String,
    proofReference: String,
    pcf: f64,
    imageId: String,
}

/*
#[derive(Serialize, Deserialize)]
pub struct StoredData {
    pub receipt: Receipt,
    pub previous_id: [u32; 8],
}

#[derive(Deserialize, Serialize)]
struct ShipmentInfo {
    activity_data_json: String,
    activity_signature: String,
    activity_public_key_pem: String,
}

#[derive(Deserialize, Serialize)]
struct Shipment {
    shipment_id: String,
    info: ShipmentInfo,
}
*/

const TOPIC_IN: &str = "shipments";
const TOPIC_OUT: &str = "pcf-results";

async fn process_payload(payload_str: &str) -> Option<ProofResponse> {
    println!("Rohdaten der Nachricht: {}", payload_str);
    // Versuch direkt zu parsen (raw JSON)
    if let Ok(proof_response) = try_handle_raw_json(payload_str).await {
        return Some(proof_response);
    }

    // Falls das fehlschlägt, versuche es als stringifizierten JSON-String zu entpacken
    let inner_json_str: String = match serde_json::from_str(payload_str) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Fehler beim Entpacken des JSON-Strings: {}", e);
            return None;
        }
    };

    try_handle_raw_json(&inner_json_str).await.ok()
}

async fn try_handle_raw_json(shipments_json: &str) -> Result<ProofResponse, ()> {
    match handle_kafka_message(shipments_json).await {
        Some(resp) => Ok(resp),
        None => Err(()),
    }
}

#[tokio::main]
async fn main() {
    let brokers = std::env::var("KAFKA_BROKER").unwrap_or_else(|_| "localhost:9092".to_string());
    env_logger::init();

    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &brokers)
        .set("security.protocol", "PLAINTEXT")
        .set("group.id", "risc0-pcf-kafka-group")
        .set("auto.offset.reset", "earliest")
        .set("enable.auto.commit", "true")
        .set("auto.commit.interval.ms", "5000")
        .create()
        .expect("Consumer creation failed");

    consumer.subscribe(&[TOPIC_IN]).unwrap();

    let producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", &brokers)
        .set("security.protocol", "PLAINTEXT")
        .create()
        .expect("Producer creation failed");

    loop {
        match consumer.recv().await {
            Ok(message) => {
                match message.payload_view::<str>() {
                    Some(Ok(payload_str)) => {
                        if let Some(proof_response) = process_payload(payload_str).await {
                            let result_json = serde_json
                                ::to_string(&proof_response)
                                .expect("Failed to serialize proof_response");
                            print!("{}", result_json);
                            let record = FutureRecord::to(TOPIC_OUT)
                                .payload(&result_json)
                                .key("some-key");
                            let _ = producer.send(record, Duration::from_secs(10)).await;
                        } else {
                            info!("Ungültige Nachricht wurde ignoriert.");
                        }
                    }
                    Some(Err(e)) => eprintln!("Payload UTF-8 error: {}", e),
                    None => eprintln!("No payload"),
                }
            }
            Err(e) => eprintln!("Kafka error receiving message: {:?}", e),
        }
    }
}

async fn handle_kafka_message(shipments_json: &str) -> Option<ProofResponse> {
    println!("Rohdaten der Nachricht: {}", &shipments_json);

    let proofing_document: ProofingDocument = match serde_json::from_str(shipments_json) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Ungültige Nachricht ignoriert (JSON Fehler): {}", e);
            return None;
        }
    };

    let env = match
        ExecutorEnv::builder()
            .write(&proofing_document)
            .and_then(|b| b.build())
    {
        Ok(env) => env,
        Err(e) => {
            eprintln!("Fehler beim Erstellen der ExecutorEnv: {}", e);
            return None;
        }
    };

    let prover = default_prover();
    println!("ELF size: {}", GUEST_PROOFING_LOGIC_ELF.len());
    let prove_info = match prover.prove(env, GUEST_PROOFING_LOGIC_ELF) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("Fehler beim Prove: {}", e);
            return None;
        }
    };

    let receipt = prove_info.receipt;

    let journal_output: f64 = match receipt.journal.decode() {
        Ok(val) => val,
        Err(e) => {
            eprintln!("Fehler beim Dekodieren des Journals: {}", e);
            return None;
        }
    };

    if let Err(e) = receipt.verify(GUEST_PROOFING_LOGIC_ID) {
        eprintln!("Receipt Verification failed: {}", e);
        return None;
    }

    let receipt_bytes = match bincode::serialize(&receipt) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("Fehler beim Serialisieren des Receipts: {}", e);
            return None;
        }
    };

    let encoded_receipt = general_purpose::STANDARD.encode(receipt_bytes);

    println!("Journal output: {}", journal_output);

    println!("Handed over response ...");

    Some(ProofResponse {
        productFootprintId: proofing_document.productFootprint.id,
        proofReceipt: encoded_receipt,
        proofReference: "123".to_string(),
        pcf: journal_output,
        imageId: format!("{:?}", GUEST_PROOFING_LOGIC_ID),
    })
}

/*
async fn prove_plain_ad(Json(shipments): Json<Vec<Shipment>>) -> Json<ProofResponse> {
    // Initialize tracing (optional; remove if you initialize globally)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let env = ExecutorEnv::builder()
        .write(&shipments)
        .unwrap()
        .build()
        .unwrap();

    let prover = default_prover();
    let prove_info = prover.prove(env, GUEST_PROOFING_LOGIC_ELF).unwrap();
    let receipt = prove_info.receipt;

    // Decode output from receipt journal
    let journal_output: f32 = receipt.journal.decode().unwrap();

    // Verify the receipt (optional here, but shows it works)
    receipt.verify(GUEST_PROOFING_LOGIC_ID).unwrap();

    // Serialize receipt (as base64-encoded bytes)
    let receipt_bytes = bincode::serialize(&receipt).unwrap();
    let encoded_receipt = general_purpose::STANDARD.encode(receipt_bytes);

    Json(ProofResponse {
        proof_receipt: encoded_receipt,
        journal_output,
        image_id: format!("{:?}", GUEST_PROOFING_LOGIC_ID),
    })
}
*/

#[cfg(test)]
mod tests {
    use super::handle_kafka_message;
    use crate::{ ProofResponse };
    use tokio;
    use std::fs;

    #[tokio::test]
    async fn smoke_test_with_realistic_shipments_json() -> Result<(), Box<dyn std::error::Error>> {
        let json_content = fs::read_to_string("src/shipment_3.json")?;

        // Call kafka handler
        let resp: ProofResponse = handle_kafka_message(&json_content).await.expect(
            "kafka_handler_failed"
        );
        assert!(!resp.proof_receipt.is_empty(), "receipt must be generated");
        assert!(resp.journal_output.is_finite(), "journal_output must be numeric");
        assert!(!resp.image_id.is_empty(), "image_id must be present");
        println!("{}", resp.journal_output);
        Ok(())
    }
}
