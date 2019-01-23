extern crate futures;
extern crate tokio;
extern crate websocket;

pub mod types;

use tokio::runtime;
use tokio::runtime::TaskExecutor;

use std::collections::HashMap;
use std::fmt::Debug;
use std::time::{Duration, Instant};

use websocket::message::OwnedMessage;
use websocket::server::InvalidConnection;
use websocket::server::r#async::Server;
use websocket::result::WebSocketError;

use futures::{Future,Stream,Sink};
use futures::future::{self, Loop};

use std::sync::{Arc,RwLock};

fn main() {
    let runtime  = runtime::Builder::new().build().unwrap();
    let executor = runtime.executor();
    let server   = Server::bind("127.0.0.1:8080", &runtime.reactor()).expect("Failed to create server");

    // Hashmap to store a sink value with an id key
    // A sink is used to send data to an open client connection
    let connections = Arc::new(RwLock::new(HashMap::new()));
    // Hashmap of id:entity pairs. This is basically the game state
    let entities    = Arc::new(RwLock::new(HashMap::new()));
    // Used to assign a unique id to each new player
    let counter     = Arc::new(RwLock::new(0));

    // Clone references to these states in order to move into the connection_handler closure
    let connections_inner = connections.clone();
    let entities_inner    = entities.clone();
    let executor_inner    = executor.clone();

    // This stream spawns a future on each new connection request from a client
    // The interesting part is the closure in "for_each" (line 46)
    let connection_handler = server.incoming()
        .map_err(|InvalidConnection { error, .. }| error)
        .for_each(move |(upgrade, addr)| {
            // Clone again to move into closure "f"
            let connections_inner = connections_inner.clone();
            let entities          = entities_inner.clone();
            let counter_inner     = counter.clone();
            let executor_2inner   = executor_inner.clone();

            // This future completes the connection and then proceses the sink and stream
            let accept = upgrade.accept().and_then(move |(framed,_)| {
                let (sink, stream) = framed.split();

                // Put a shareable (multiple-producer) channel ontop of the sink
                let (tx, rx) = futures::sync::mpsc::channel(100);
                let f = rx
                    .map_err(|_| WebSocketError::ResponseError("rx dropped"))
                    .forward(sink)
                    .map(|_| ())
                    .map_err(|_| ());
                let sink = tx;

                executor_2inner.spawn(f);

                { // Increment the counter by first locking the RwLock
                    let mut c = counter_inner.write().unwrap();
                    *c += 1;
                }

                // Assign an id to the new connection and associate with a new entity and the sink
                let id = *counter_inner.read().unwrap();
                connections_inner.write().unwrap().insert(id,sink);
                entities.write().unwrap().insert(id, types::Entity{id, pos:(0,0)} ); // Start at position 0
                let c = *counter_inner.read().unwrap();

                // Spawn a stream to process future messages from this client
                let entities_inner = entities.clone();
                let f = stream
                    .map_err(|_| ())
                    .for_each(move |msg| {
                        process_message(c, &msg, entities.clone());
                        Ok(())
                    })
                    .then(move |_| {
                        connections_inner.write().unwrap().remove(&id);
                        entities_inner.write().unwrap().remove(&id);
                        Ok(())
                    });

                executor_2inner.spawn(f);

                Ok(())
            }).map_err(|_| ());

            executor_inner.spawn(accept);
            Ok(())
        })
        .map_err(|_| ());

    // This stream is the game loop
    let send_handler = tokio::timer::Interval::new(Instant::now(), Duration::from_millis(100))
        .map_err(|_| ())
        .for_each(move |_| {
            let connections_inner = connections.clone();
            let executor          = executor.clone();
            let entities_inner    = entities.clone();

            let entities = entities_inner.read().unwrap();
            if let Some((_, first)) = entities.iter().take(1).next() {
                // Meticulously serialize entity vector into json
                let serial_entities = format!("[{}]", entities.iter().skip(1)
                                              .map(|(_,e)| e.to_json())
                                              .fold(first.to_json(), |acc,s| format!("{},{}",s,acc)));

                let connections = connections_inner.read().unwrap();
                for sink in connections.values().cloned() {
                    // This is where the game state is actually sent to the client
                    let f = sink
                        .send(OwnedMessage::Text(serial_entities.clone()))
                        .map(|_| ())
                        .map_err(|_| ());

                    executor.spawn(f);
                };
            }

            Ok(())
        });

    // Finally, block the main thread to wait for the connection_handler and send_handler streams
    // to finish. Which they never should unless there is an error
    runtime
        .block_on_all(connection_handler.select(send_handler))
        .map_err(|_| println!("Error while running core loop"))
        .unwrap();
}

// Update a player's entity state depending on the command they sent
fn process_message(
    id: u32,
    msg: &OwnedMessage,
    entities: Arc<RwLock<HashMap<u32,types::Entity>>>
) {
    if let OwnedMessage::Text(ref txt) = *msg {
        // For fun
        println!("Received msg '{}' from id {}", txt, id);

        if txt == "right" {
            entities.write().unwrap().entry(id).and_modify(|e| { e.pos.0 += 10 });
        }
        else if txt == "left" {
            entities.write().unwrap().entry(id).and_modify(|e| { e.pos.0 -= 10 });
        }
        else if txt == "down" {
            entities.write().unwrap().entry(id).and_modify(|e| { e.pos.1 += 10 });
        }
        else if txt == "up" {
            entities.write().unwrap().entry(id).and_modify(|e| { e.pos.1 -= 10 });
        }
    }
}
