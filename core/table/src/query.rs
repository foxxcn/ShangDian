use std::net::SocketAddr;

use fleek_crypto::NodeNetworkingPublicKey;
use serde::{Deserialize, Serialize};

use crate::table::TableKey;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeInfo {
    pub address: SocketAddr,
    pub key: NodeNetworkingPublicKey,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Query {
    Find { find_value: bool, target: TableKey },
    Store { key: TableKey, value: Vec<u8> },
    Ping,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum MessagePayload {
    Query(Query),
    Response(Response),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Message {
    // Random value used that must be returned in response.
    pub id: u64,
    // Channel on which to route the response.
    pub channel_id: u64,
    // Sender's public key.
    pub sender_key: NodeNetworkingPublicKey,
    // Payload of message.
    pub payload: MessagePayload,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    pub nodes: Vec<NodeInfo>,
    pub value: Option<Vec<u8>>,
}
