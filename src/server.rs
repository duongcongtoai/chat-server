use chatinfra::chat_service_client::ChatServiceClient;
use chatinfra::chat_service_server::{ChatService, ChatServiceServer};
use chatinfra::{
    event, Event, EventPing, MessageResponse, PingRequest, PingResponse, SeenMessageRequest,
    SeenMessageResponse, SendMessageRequest, SendMessageResponse, SubscribeRequest,
    SubscribeResponse,
};
use drop::DropReceiver;
use fnv::FnvHasher;
use std::collections::HashMap;
use std::hash::Hasher;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{self, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub mod chatinfra {
    tonic::include_proto!("chatinfra");
}
mod drop;

struct ChatSvc {
    hubs: Vec<Hub>,
}

struct BroadcastConn {
    channel: broadcast::Sender<Message>,
    total_client: i32,
}
struct Registry {
    client_stream: mpsc::Sender<Result<Event, Status>>,
    close_chan: UnboundedReceiver<usize>,
    session_id: String,
    user_id: String,
}

#[derive(Clone, Debug)]
struct Message {
    user_id: String,
    msg: event::Event,
}

struct Hub {
    new_msg_sender: mpsc::Sender<Message>,
    new_registry_sender: mpsc::Sender<Registry>,
}
struct MultipleHub {
    hubs: Arc<Vec<Hub>>,
    length: u64,
}

impl MultipleHub {
    async fn new() -> Self {
        let mut hubs = Vec::new();
        for i in 0..64 {
            hubs.push(Hub::new().await);
        }
        MultipleHub {
            hubs: Arc::new(hubs),
            length: 64,
        }
    }

    fn index(ch: &String, bucket_length: u64) -> u64 {
        let mut hasher = FnvHasher::with_key(14695981039346656037);
        hasher.write(ch.as_bytes());
        hasher.finish() % bucket_length
    }

    fn get_hub(&self, ch: &String) -> &Hub {
        let index = Self::index(&ch, self.length);
        self.hubs.get(index as usize).unwrap()
    }
}

#[tonic::async_trait]
impl ChatService for MultipleHub {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        let hub = self.get_hub(&request.get_ref().user_id);
        hub.ping(request).await
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        let hub = self.get_hub(&request.get_ref().receiver_id);
        hub.send_message(request).await
    }

    async fn seen_message(
        &self,
        request: Request<SeenMessageRequest>,
    ) -> Result<Response<SeenMessageResponse>, Status> {
        Ok(Response::new(SeenMessageResponse {}))
    }

    type SubscribeStream = DropReceiver<Result<Event, Status>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let hub = self.get_hub(&request.get_ref().user_id);
        hub.subscribe(request).await
    }
}

#[tonic::async_trait]
impl ChatService for Hub {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        let ret = self
            .new_msg_sender
            .send(Message {
                user_id: request.get_ref().user_id.clone(),
                msg: event::Event::EventPing(EventPing {
                    session_id: request.get_ref().session_id.clone(),
                }),
            })
            .await;
        match ret {
            Ok(()) => Ok(Response::new(PingResponse {})),
            Err(send_err) => Err(Status::new(
                tonic::Code::Internal,
                format!("err sending ping to internal loop:{}", send_err.to_string()),
            )),
        }
    }
    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        let ret = self
            .new_msg_sender
            .send(Message {
                user_id: request.get_ref().receiver_id.clone(),
                msg: event::Event::EventNewMessage(MessageResponse {
                    user_id: request.get_ref().sender_id.clone(),
                    content: request.get_ref().message.clone(),
                    created_at: None,
                }),
            })
            .await;
        match ret {
            Ok(()) => Ok(Response::new(SendMessageResponse {})),
            Err(send_err) => Err(Status::new(
                tonic::Code::Internal,
                format!(
                    "err broadcasting to internal channel {}",
                    send_err.to_string()
                ),
            )),
        }
    }
    async fn seen_message(
        &self,
        request: Request<SeenMessageRequest>,
    ) -> Result<Response<SeenMessageResponse>, Status> {
        Ok(Response::new(SeenMessageResponse {}))
    }

    type SubscribeStream = DropReceiver<Result<Event, Status>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        println!("subscribe = {:?}", request);

        let (tx, rx) = mpsc::channel(4);
        let (to_return, close_chan) = DropReceiver::new(rx);

        let session_id = Uuid::new_v4();

        let ret = self
            .new_registry_sender
            .send(Registry {
                client_stream: tx,
                session_id: session_id.to_string(),
                user_id: request.get_ref().user_id.clone(),
                close_chan,
            })
            .await;
        match ret {
            Ok(()) => Ok(Response::new(to_return)),
            Err(send_err) => Err(Status::new(
                tonic::Code::Internal,
                format!(
                    "err broadcasting to internal registration channel {}",
                    send_err.to_string()
                ),
            )),
        }
    }
}

impl Hub {
    async fn new() -> Self {
        let (new_registry_sender, new_registry_chan) = mpsc::channel::<Registry>(1);
        let (new_msg_sender, new_msg_chan) = mpsc::channel::<Message>(1);
        let (deregistry_sender, deregistry_chan) = mpsc::channel::<Registry>(1);
        let ret = Hub {
            new_msg_sender,
            new_registry_sender,
        };
        tokio::spawn(async move {
            Self::start(
                deregistry_sender,
                new_registry_chan,
                new_msg_chan,
                deregistry_chan,
            )
            .await
        });
        ret
    }
    async fn start(
        deregistry_sender: mpsc::Sender<Registry>,
        mut new_registry_chan: mpsc::Receiver<Registry>,
        mut new_msg_chan: mpsc::Receiver<Message>,
        mut deregistry_chan: mpsc::Receiver<Registry>,
    ) {
        let mut conns_list = HashMap::<String, BroadcastConn>::new();
        loop {
            tokio::select! {
                maybe_msg = new_msg_chan.recv() => {
                     match maybe_msg{
                         None => unimplemented!(),
                         Some(msg) => {
                             match conns_list.get(&msg.user_id) {
                                None => {unimplemented!("new msg with user id but no connection found")},
                                Some(cons) => {
                                    cons.channel.send(msg).unwrap();
                                },
                             }
                         }
                    };
                }
                to_be_deleted = deregistry_chan.recv() => {
                    match to_be_deleted {
                        None => unimplemented!(),
                        Some(to_be_deleted) => {
                            match conns_list.get_mut(&to_be_deleted.user_id) {
                                None => { // something wrong here
                                },
                                Some(mut broadcast_chann) => {
                                    broadcast_chann.total_client-=1;
                                    if broadcast_chann.total_client <= 0{
                                        conns_list.remove(&to_be_deleted.user_id);
                                    }
                                    println!("{} disconnected;",to_be_deleted.user_id);
                                }
                            }
                        }
                    }
                }
                new_registry = new_registry_chan.recv() => {
                    match new_registry {
                        None => unimplemented!(),
                        Some(new_registry) => {
                            match conns_list.get_mut(&new_registry.user_id) {
                                None => {
                                    let (tx,  rx1) = broadcast::channel::<Message>(10);
                                    let broadcast_chann = BroadcastConn{
                                        channel: tx,
                                        total_client:1,
                                    };
                                    let user_id = new_registry.user_id.clone();
                                    let ping_msg = Message{
                                        user_id: new_registry.user_id.clone(),
                                        msg: event::Event::EventPing(EventPing{
                                            session_id: new_registry.session_id.clone(),
                                        }),
                                    };
                                    let deregistry_chan_2 = deregistry_sender.clone();
                                    tokio::spawn(async move {Self::push_client_loop(deregistry_chan_2,rx1,new_registry).await});
                                    broadcast_chann.channel.send(ping_msg).unwrap();
                                    conns_list.insert(user_id,broadcast_chann);
                                },
                                Some(mut broadcast_chann) => {
                                    let rxn = broadcast_chann.channel.subscribe();
                                    broadcast_chann.total_client +=1;
                                    let deregistry_chan_2 = deregistry_sender.clone();
                                    let ping_msg = Message{
                                        user_id: new_registry.user_id.clone(),
                                        msg: event::Event::EventPing(EventPing{
                                            session_id: new_registry.session_id.clone(),
                                        }),
                                    };
                                    tokio::spawn(async move {Self::push_client_loop(deregistry_chan_2,rxn,new_registry).await});
                                    broadcast_chann.channel.send(ping_msg).unwrap();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    /*
     * new_msg_chan receives when broadcasted by some where in the server
     * client_stream is used to push msg to client
     */
    async fn push_client_loop(
        deregistry_sender: mpsc::Sender<Registry>,
        mut new_msg_chan: broadcast::Receiver<Message>,
        mut client_registry: Registry,
    ) {
        let mut timer = time::interval(Duration::from_secs(10));
        let mut latest_ping = Instant::now();
        let mut init = false;
        // let mut pinned = &mut Pin::new(&mut client_registry.close_chan);
        loop {
            tokio::select! {
                _ = client_registry.close_chan.recv() => {
                    deregistry_sender.send(client_registry).await;
                    return
                }
                _ = timer.tick() => {
                    if Instant::now().duration_since(latest_ping) > Duration::from_millis(10000) {
                        deregistry_sender.send(client_registry).await;
                        return
                    }
                    //TODO: client latest ping, of > 10, abort
                }
                //TODO: client ping to refresh healthcheck
                maybe_msg = new_msg_chan.recv() => {
                     match maybe_msg{
                         Err(_) => unimplemented!(),
                         Ok(msg) => {
                             match msg.msg {
                                 event::Event::EventNewMessage(new_msg) => {
                                     let some_event = Event{
                                        event: Some(event::Event::EventNewMessage(MessageResponse{
                                            user_id: String::from(new_msg.user_id),
                                            content: String::from(new_msg.content),
                                            created_at: None,
                                        })),
                                    };
                                     &client_registry.client_stream.send(Ok(some_event)).await.unwrap();
                                 },
                                 event::Event::EventPing(ping_msg) => {
                                     if ping_msg.session_id == client_registry.session_id{
                                         latest_ping = Instant::now();
                                         if !init {
                                            init = true;
                                            let some_event = Event{
                                                event: Some(event::Event::EventPing(EventPing{
                                                    session_id: ping_msg.session_id.clone()
                                                })),
                                            };

                                            &client_registry.client_stream.send(Ok(some_event)).await.unwrap();
                                         }
                                     }
                                 }
                             }
                         }
                    };
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:10000".parse().unwrap();

    println!("RouteGuideServer listening on: {}", addr);
    let chathub = MultipleHub::new().await;

    let svc = ChatServiceServer::new(chathub);

    Server::builder().add_service(svc).serve(addr).await?;

    Ok(())
}
