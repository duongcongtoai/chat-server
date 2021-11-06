use chatinfra::chat_service_client::ChatServiceClient;
use chatinfra::chat_service_server::{ChatService, ChatServiceServer};
use chatinfra::{
    event, Event, EventPing, MessageResponse, PingRequest, PingResponse, SeenMessageRequest,
    SeenMessageResponse, SendMessageRequest, SendMessageResponse, SubscribeRequest,
    SubscribeResponse,
};
use std::collections::HashMap;
use std::pin::Pin;
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub mod chatinfra {
    tonic::include_proto!("chatinfra");
}
struct ChatSvc {
    hubs: Vec<Hub>,
}

/* struct Conn {
    newmsg_chan: mpsc::Receiver<Message>,
    user_id: String,
} */
struct BroadcastConn {
    channel: broadcast::Sender<Message>,
    user_id: String,
    total_client: i32,
}
struct Registry {
    client_stream: mpsc::Sender<Result<Event, Status>>,
    session_id: String,
    user_id: String,
}

#[derive(Clone, Debug)]
struct Message {
    content: String,
    user_id: String,
    msg: event::Event,
}

struct Hub {
    idx: i32,
    conn_count: i32,
    new_msg_sender: mpsc::Sender<Message>,
    new_registry_sender: mpsc::Sender<Registry>,
    deregistry_sender: mpsc::Sender<Registry>,
    /* new_msg_receiver: mpsc::Receiver<Message>,
    new_registry_receiver: mpsc::Receiver<Registry>,
    deregistry_receiver: mpsc::Receiver<Registry>, */
}

#[tonic::async_trait]
impl ChatService for Hub {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        self.new_msg_sender
            .send(Message {
                user_id: request.get_ref().user_id.clone(),
                content: String::from(""),
                msg: event::Event::EventPing(EventPing {
                    session_id: request.get_ref().session_id.clone(),
                }),
            })
            .await;
        Ok(Response::new(PingResponse {}))
    }
    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        let ret = self
            .new_msg_sender
            .send(Message {
                user_id: request.get_ref().user_id.clone(),
                content: String::from(""),
                msg: event::Event::EventNewMessage(MessageResponse {
                    user_id: request.get_ref().user_id.clone(),
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

    type SubscribeStream = ReceiverStream<Result<Event, Status>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        println!("ListFeatures = {:?}", request);

        let (tx, rx) = mpsc::channel(4);

        let session_id = Uuid::new_v4();

        let ret = self
            .new_registry_sender
            .send(Registry {
                client_stream: tx,
                session_id: session_id.to_string(),
                user_id: request.get_ref().user_id.clone(),
            })
            .await;
        match ret {
            Ok(()) => {}
            Err(send_err) => {
                return Err(Status::new(
                    tonic::Code::Internal,
                    format!(
                        "err broadcasting to internal registration channel {}",
                        send_err.to_string()
                    ),
                ));
            }
        }
        let ret = self
            .new_msg_sender
            .send(Message {
                user_id: request.get_ref().user_id.clone(),
                content: String::from(""),
                msg: event::Event::EventPing(EventPing {
                    session_id: session_id.to_string(),
                }),
            })
            .await;

        match ret {
            Ok(()) => Ok(Response::new(ReceiverStream::new(rx))),
            Err(send_err) => {
                return Err(Status::new(
                    tonic::Code::Internal,
                    format!(
                        "err broadcasting to internal message channel to send initial ping {}",
                        send_err.to_string()
                    ),
                ));
            }
        }
    }
}

impl Hub {
    async fn new() -> Self {
        let (new_registry_sender, mut new_registry_chan) = mpsc::channel::<Registry>(1);
        let (new_msg_sender, mut new_msg_chan) = mpsc::channel::<Message>(1);
        let (deregistry_sender, mut deregistry_chan) = mpsc::channel::<Registry>(1);
        let ret = Hub {
            idx: 0,
            conn_count: 0,
            new_msg_sender: new_msg_sender.clone(),
            new_registry_sender: new_registry_sender.clone(),
            deregistry_sender: deregistry_sender.clone(),
        };
        tokio::spawn(async move {
            Self::start(
                new_registry_sender.clone(),
                new_msg_sender.clone(),
                deregistry_sender.clone(),
                new_registry_chan,
                new_msg_chan,
                deregistry_chan,
            )
        });
        ret
    }
    async fn start(
        new_registry_sender: mpsc::Sender<Registry>,
        new_msg_sender: mpsc::Sender<Message>,
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
                                  None => {},
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
                                    conns_list.insert(String::from(&new_registry.user_id),BroadcastConn{
                                        user_id: String::from(&new_registry.user_id),
                                        channel: tx,
                                        total_client:1,
                                    });
                                    let deregistry_chan_2 = deregistry_sender.clone();
                                    tokio::spawn(async move {Self::push_client_loop(deregistry_chan_2,rx1,new_registry).await});
                                },
                                Some(mut broadcast_chann) => {
                                    let  rxn = broadcast_chann.channel.subscribe();
                                    broadcast_chann.total_client +=1;
                                    let deregistry_chan_2 = deregistry_sender.clone();
                                    tokio::spawn(async move {Self::push_client_loop(deregistry_chan_2,rxn,new_registry).await});
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
        client_registry: Registry,
    ) {
        let mut timer = time::interval(Duration::from_secs(10));
        let mut latest_ping = Instant::now();
        loop {
            tokio::select! {
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
                                            user_id: String::from(&client_registry.user_id),
                                            content: String::from(msg.content),
                                            created_at: None,
                                        })),
                                    };
                                     &client_registry.client_stream.send(Ok(some_event)).await.unwrap();
                                 },
                                 event::Event::EventPing(ping_msg) => {
                                     if ping_msg.session_id == client_registry.session_id{
                                         latest_ping = Instant::now();
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
    let chathub = Hub::new().await;

    let svc = ChatServiceServer::new(chathub);

    Server::builder().add_service(svc).serve(addr).await?;

    Ok(())
}
