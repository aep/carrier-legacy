use carrier::channel;
use carrier::endpoint;
use failure::Error;
use futures::sync::mpsc;
use futures::sync::oneshot;
use futures::{self, Future, Sink, Stream};
use futurize;
use gcmap::{HashMap, MarkOnDrop};
use headers::Headers;
use identity;
use proto;
use ptrmap;
use std::net::SocketAddr;
use tokio;
use xlog;
use stats;
use std::collections::HashSet;

macro_rules! wrk_try {
    ($self:ident, $x:expr) => {
        match { $x } {
            Err(e) => return Box::new(futures::future::err((Some($self), e))),
            Ok(v) => v,
        }
    };
}

macro_rules! wrk_continue {
    ($self:ident, $x:expr) => {
        Box::new($x.then(|m| match m {
            Err(e) => Err((Some($self), e)),
            Ok(v) => Ok((Some($self), v)),
        }))
    };
}

// ---------
// a shadow coordinates publishers and subscribers on an address
// -----------

pub mod shadow {
    #[derive(Worker)]
    pub enum Command {
        #[returns = "usize"]
        Subscribe {
            identity: super::identity::Identity,
            msg:      super::proto::SubscribeRequest,
            rpc:      super::mpsc::Sender<super::proto::SubscribeChange>,
        },
        #[returns = "usize"]
        Publish {
            identity: super::identity::Identity,
            msg:      super::proto::PublishRequest,
            rpc:      super::mpsc::Sender<super::proto::PublishChange>,
            ipaddr:   super::SocketAddr,
        },
        Unsubscribe {
            ptr: usize,
        },
        Unpublish {
            ptr: usize,
        },
    }
}

#[derive(Clone)]
pub struct Subscriber {
    rpc: mpsc::Sender<proto::SubscribeChange>,
}

#[derive(Clone)]
pub struct Publisher {
    rpc:    mpsc::Sender<proto::PublishChange>,
    xaddr:  identity::SignedAddress,
    ipaddr: SocketAddr,
}

//TODO there could be millions of publishers. this datastructure wont scale well
struct Shadow {
    address: identity::Address,
    #[allow(dead_code)]
    mark: MarkOnDrop,

    subscribers: ptrmap::PtrMap<identity::Identity, Subscriber>,
    publishers:  ptrmap::PtrMap<identity::Identity, Publisher>,
}

impl shadow::Worker for Shadow {
    fn subscribe(
        mut self,
        identity: identity::Identity,
        _msg: proto::SubscribeRequest,
        rpc: mpsc::Sender<proto::SubscribeChange>,
    ) -> Box<Future<Item = (Option<Self>, usize), Error = (Option<Self>, Error)> + Send + Sync> {
        info!("[{}] new subscriber {}", self.address, identity);
        let (ptr, old) = self.subscribers.insert(identity, Subscriber { rpc: rpc.clone() });

        if let Some(old) = old {
            debug!("migrating old subscriber");
            let old = old
                .rpc
                .send(proto::SubscribeChange {
                    m: Some(proto::subscribe_change::M::Supersede(proto::Supersede {})),
                }).map_err(|e| warn!("{}", e))
                .map(|_| ());
            tokio::spawn(old);
        };

        let publishers: Vec<(identity::Identity, Publisher)> =
            self.publishers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let ft = futures::stream::iter_ok(publishers.into_iter())
            .fold(rpc, |rpc, (identity, publisher)| {
                rpc.send(proto::SubscribeChange {
                    m: Some(proto::subscribe_change::M::Publish(proto::Publish {
                        identity: identity.as_bytes().to_vec(),
                        xaddr:    publisher.xaddr.to_vec(),
                    })),
                }).map_err(|e| error!("{}", e))
            }).map(|_| ());
        tokio::spawn(ft);

        let ft = futures::future::ok((Some(self), ptr));
        Box::new(ft)
    }

    fn publish(
        mut self,
        identity: identity::Identity,
        msg: proto::PublishRequest,
        rpc: mpsc::Sender<proto::PublishChange>,
        ipaddr: SocketAddr,
    ) -> Box<Future<Item = (Option<Self>, usize), Error = (Option<Self>, Error)> + Send + Sync> {
        let xaddr = wrk_try!(self, identity::SignedAddress::from_bytes(msg.xaddr));

        let (mark, old) = self.publishers.insert(
            identity.clone(),
            Publisher {
                xaddr: xaddr.clone(),
                rpc: rpc.clone(),
                ipaddr,
            },
        );
        info!("[{}] new publisher {} {:#x}", self.address, identity, mark);

        let identity_ = identity.clone();
        if let Some(old) = old {
            debug!("migrating old publisher");

            let subscribers: Vec<(identity::Identity, Subscriber)> =
                self.subscribers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let ft = futures::stream::iter_ok(subscribers.into_iter())
                .for_each(move |(_, subscriber)| {
                    subscriber
                        .rpc
                        .clone()
                        .send(proto::SubscribeChange {
                            m: Some(proto::subscribe_change::M::Unpublish(proto::Unpublish {
                                identity: identity_.as_bytes().to_vec(),
                            })),
                        }).map(|_| ())
                        .map_err(|e| error!("{}", e))
                }).map(|_| ());
            tokio::spawn(ft);

            let old = old
                .rpc
                .send(proto::PublishChange {
                    m: Some(proto::publish_change::M::Supersede(proto::Supersede {})),
                }).map_err(|e| warn!("{}", e))
                .map(|_| ());
            tokio::spawn(old);
        };

        let subscribers: Vec<(identity::Identity, Subscriber)> =
            self.subscribers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let ft = futures::stream::iter_ok(subscribers.into_iter())
            .for_each(move |(_, subscriber)| {
                subscriber
                    .rpc
                    .clone()
                    .send(proto::SubscribeChange {
                        m: Some(proto::subscribe_change::M::Publish(proto::Publish {
                            identity: identity.as_bytes().to_vec(),
                            xaddr:    xaddr.to_vec(),
                        })),
                    }).map(|_| ())
                    .map_err(|e| error!("{}", e))
            }).map(|_| ());
        tokio::spawn(ft);

        let ft = futures::future::ok((Some(self), mark));
        Box::new(ft)
    }

    fn unsubscribe(
        mut self,
        ptr: usize,
    ) -> Box<Future<Item = (Option<Self>, ()), Error = (Option<Self>, Error)> + Send + Sync> {
        if let Some((identity, _subscriber)) = self.subscribers.remove_ptr(ptr) {
            debug!("[{}] unsubscribe {}", self.address, identity);
        }

        if self.subscribers.len() == 0 && self.publishers.len() == 0 {
            debug!("shadow worker {} stopped because no pub/sub left", self.address);
            Box::new(futures::future::ok((None, ())))
        } else {
            Box::new(futures::future::ok((Some(self), ())))
        }
    }

    fn unpublish(
        mut self,
        ptr: usize,
    ) -> Box<Future<Item = (Option<Self>, ()), Error = (Option<Self>, Error)> + Send + Sync> {
        if let Some((identity, _publisher)) = self.publishers.remove_ptr(ptr) {
            debug!("[{}] unpublish {} {:#x}", self.address, identity, ptr);

            let subscribers: Vec<(identity::Identity, Subscriber)> =
                self.subscribers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let ft = futures::stream::iter_ok(subscribers.into_iter())
                .for_each(move |(_, subscriber)| {
                    subscriber
                        .rpc
                        .clone()
                        .send(proto::SubscribeChange {
                            m: Some(proto::subscribe_change::M::Unpublish(proto::Unpublish {
                                identity: identity.as_bytes().to_vec(),
                            })),
                        }).map(|_| ())
                        .map_err(|e| error!("{}", e))
                }).map(|_| ());
            tokio::spawn(ft);
        }

        if self.subscribers.len() == 0 && self.publishers.len() == 0 {
            debug!("shadow worker {} stopped because no pub/sub left", self.address);
            Box::new(futures::future::ok((None, ())))
        } else {
            Box::new(futures::future::ok((Some(self), ())))
        }
    }
}

// ---------
// the shadow broker maintains all shadows
// -----------

pub mod broker {
    #[derive(Worker)]
    pub enum Command {
        #[returns = "super::ptrmap::DropHook"]
        Subscribe {
            identity: super::identity::Identity,
            msg:      super::proto::SubscribeRequest,
            rpc:      super::mpsc::Sender<super::proto::SubscribeChange>,
        },
        #[returns = "(super::MarkOnDrop, super::ptrmap::DropHook)"]
        Publish {
            identity: super::identity::Identity,
            peer:     super::peer::Handle,
            msg:      super::proto::PublishRequest,
            rpc:      super::mpsc::Sender<super::proto::PublishChange>,
            ipaddr:   super::SocketAddr,
        },
        #[returns = "Option<(super::peer::Handle, super::SocketAddr)>"]
        GetPeer { identity: super::identity::Identity },
    }
}

struct Broker {
    shadows: HashMap<identity::Address, shadow::Handle>,
    peers:   HashMap<identity::Identity, (peer::Handle, SocketAddr)>,
}

impl Broker {
    fn shadow(&mut self, address: identity::Address) -> &mut shadow::Handle {
        self.shadows.entry(address.clone()).or_insert_with(|mark| {
            let (worker, handle) = shadow::spawn(
                100,
                Shadow {
                    mark,
                    address,
                    subscribers: ptrmap::PtrMap::new(),
                    publishers: ptrmap::PtrMap::new(),
                },
            );
            tokio::spawn(worker);
            handle
        })
    }
}

impl broker::Worker for Broker {

    fn subscribe(
        mut self,
        identity: identity::Identity,
        msg: proto::SubscribeRequest,
        rpc: mpsc::Sender<proto::SubscribeChange>,
    ) -> Box<Future<Item = (Option<Self>, ptrmap::DropHook), Error = (Option<Self>, Error)> + Send + Sync> {
        let shadow = wrk_try!(self, identity::Address::from_bytes(&msg.shadow));
        let mut shadow = self.shadow(shadow).clone();

        let ft = shadow.subscribe(identity, msg, rpc).and_then(|ptr| {
            let hook = ptrmap::DropHook::new(move || {
                tokio::spawn(shadow.unsubscribe(ptr).map_err(|e| error!("{}", e)));
            });
            Ok(hook)
        });

        wrk_continue!(self, ft)
    }

    fn publish(
        mut self,
        identity: identity::Identity,
        peer: peer::Handle,
        msg: proto::PublishRequest,
        rpc: mpsc::Sender<proto::PublishChange>,
        ipaddr: SocketAddr,
    ) -> Box<Future<Item = (Option<Self>, (MarkOnDrop, ptrmap::DropHook)), Error = (Option<Self>, Error)> + Send + Sync>
    {
        let shadow = wrk_try!(self, identity::Address::from_bytes(&msg.shadow));
        let mut shadow = self.shadow(shadow).clone();

        let (gcmark, _) = self.peers.insert(identity.clone(), (peer, ipaddr.clone()));

        let ft = shadow.publish(identity, msg, rpc, ipaddr).and_then(|ptr| {
            let hook = ptrmap::DropHook::new(move || {
                tokio::spawn(shadow.unpublish(ptr).map_err(|e| error!("{}", e)));
            });
            Ok((gcmark, hook))
        });

        wrk_continue!(self, ft)
    }

    fn get_peer(
        mut self,
        identity: identity::Identity,
    ) -> Box<
        Future<Item = (Option<Self>, Option<(peer::Handle, SocketAddr)>), Error = (Option<Self>, Error)> + Send + Sync,
    > {
        let ft = futures::future::ok(self.peers.get(&identity).cloned());
        wrk_continue!(self, ft)
    }
}

pub(crate) fn spawn() -> broker::Handle {
    let worker = Broker {
        shadows: HashMap::new(),
        peers:   HashMap::new(),
    };
    let (worker, handle) = broker::spawn(100, worker);
    tokio::spawn(worker);
    handle
}

// ---------
// rpc
// -----------
pub mod peer {
    #[derive(Worker)]
    pub enum Command {
        #[returns = "super::proto::PeerConnectResponse"]
        Connect { req: super::proto::PeerConnectRequest },
    }
}

struct Peer {
    channel: channel::Channel,
    ipaddr:  SocketAddr,
}

impl peer::Worker for Peer {
    fn connect(
        mut self,
        req: proto::PeerConnectRequest,
    ) -> Box<Future<Item = (Option<Self>, proto::PeerConnectResponse), Error = (Option<Self>, Error)> + Send + Sync>
    {
        let selfipaddr = self.ipaddr.clone();
        let ft = self
            .channel
            .message("/carrier.broker.v1/peer/connect")
            .unwrap()
            .send(req)
            .flatten_stream()
            .into_future()
            .map_err(|(e, _)| e)
            .and_then(move |(resp, _)| {
                let mut resp: proto::PeerConnectResponse = resp.expect("eof before header");
                resp.paths.push(proto::Path {
                    category: (proto::path::Category::Internet as i32),
                    ipaddr:   format!("{}", selfipaddr),
                });
                Ok(resp)
            });

        wrk_continue!(self, ft)
    }
}

struct Srv {
    endpoint:       endpoint::Endpoint,
    broker:         broker::Handle,
    identity:       identity::Identity,
    worker:         peer::Handle,
    ipaddr:         SocketAddr,
    coordinators:   HashSet<identity::Identity>,
    epoch:          u64,
}

impl broker::Handle {
    pub fn dispatch(
        &self,
        endpoint: endpoint::Endpoint,
        mut channel: channel::Channel,
        ipaddr: SocketAddr,
        coordinators: HashSet<identity::Identity>,
    ) -> impl Future<Item = (), Error = Error> {
        let lst = channel.listener().unwrap();
        let identity = channel.identity().clone();

        let (worker, handle) = peer::spawn(
            100,
            Peer {
                channel,
                ipaddr: ipaddr.clone(),
            },
        );
        tokio::spawn(worker);

        let srv = Srv {
            broker: self.clone(),
            identity,
            worker: handle,
            endpoint,
            ipaddr,
            coordinators,
            epoch: 0,
        };
        proto::Broker::dispatch(lst, srv)
    }
}

impl proto::Broker::Service for Srv {
    fn epochsync(
        &mut self,
        _headers: Headers,
        msg: proto::EpochSyncRequest,
    ) -> Result<Box<Future<Item = proto::EpochSyncResponse, Error = Error> + Sync + Send + 'static>, Error> {

        // check if the sender is a trusted coordinator
        if !self.coordinators.contains(&self.identity) {
            return Ok(Box::new(futures::future::ok(proto::EpochSyncResponse::default())));
        }


        info!("epoch sync from {} to {} by coordinator {}",
              self.epoch, msg.epoch, self.identity);

        let clear = {
            if self.epoch != msg.epoch {
                self.epoch = msg.epoch;
                true
            } else {
                false
            }
        };


        let (r_tx, r_rx) = oneshot::channel();
        let ft = self
            .endpoint.work.clone().send(endpoint::EndpointWorkerCmd::DumpStats(r_tx, clear))
            .map_err(Error::from)
            .and_then(move |_| {
                r_rx
                    .map_err(Error::from)
                    .and_then(|dump|{
                        Ok(proto::EpochSyncResponse{
                            dump: Some(dump),
                        })
                    })
            });

        Ok(Box::new(ft))
    }

    fn subscribe(
        &mut self,
        _headers: Headers,
        msg: proto::SubscribeRequest,
    ) -> Result<Box<Stream<Item = proto::SubscribeChange, Error = Error> + Sync + Send + 'static>, Error> {
        let (tx, rx) = mpsc::channel(100);

        let ft = self
            .broker
            .subscribe(self.identity.clone(), msg, tx)
            .and_then(|mark| {
                let rx = rx.map_err(|()| unreachable!());
                Ok(futurize::mark_stream(rx, mark))
            }).into_stream()
            .flatten();

        Ok(Box::new(ft))
    }

    fn publish(
        &mut self,
        _headers: Headers,
        msg: proto::PublishRequest,
    ) -> Result<Box<Stream<Item = proto::PublishChange, Error = Error> + Sync + Send + 'static>, Error> {
        let (tx, rx) = mpsc::channel(100);

        let ft = self
            .broker
            .publish(self.identity.clone(), self.worker.clone(), msg, tx, self.ipaddr.clone())
            .and_then(|mark| {
                let rx = rx.map_err(|()| unreachable!());
                Ok(futurize::mark_stream(rx, mark))
            }).into_stream()
            .flatten();

        Ok(Box::new(ft))
    }

    fn connect(
        &mut self,
        _headers: Headers,
        msg: proto::ConnectRequest,
    ) -> Result<Box<Stream<Item = proto::ConnectResponse, Error = Error> + Sync + Send + 'static>, Error> {
        if !xlog::advance(&self.identity, msg.timestamp as u64) {
            warn!("cannot accept connect handshake: reused timestamp {}", msg.timestamp);
            let ft = futures::stream::once(Ok(proto::ConnectResponse {
                ok:        false,
                handshake: Vec::new(),
                route:     0,
                paths:     Vec::new(),
            }));
            return Ok(Box::new(ft));
        }

        let msgtimestamp = msg.timestamp;
        let msghandshake = msg.handshake;
        let msgidentity = msg.identity;
        let mut paths = msg.paths;
        paths.push(proto::Path {
            category: (proto::path::Category::Internet as i32),
            ipaddr:   format!("{}", self.ipaddr),
        });

        let selfipaddr = self.ipaddr.clone();
        let selfidentity = self.identity.as_bytes().to_vec();
        let selfidentity_ = self.identity.clone();
        let mut endpoint = self.endpoint.clone();

        let mut broker = self.broker.clone();
        let ft = futures::future::result(identity::Identity::from_bytes(msgidentity))
            .and_then(move |target| {
                let tc = stats::PacketCounter{
                    initiator: Some(selfidentity_),
                    responder: Some(target.clone()),
                    ..stats::PacketCounter::default()
                };
                broker.get_peer(target).and_then(move |maybe| {
                    if let Some((mut peer, ipaddr)) = maybe {

                        let ft = endpoint
                            .proxy(selfipaddr, ipaddr, tc)
                            .and_then(move |proxy| {
                                peer.connect(proto::PeerConnectRequest {
                                    identity:  selfidentity,
                                    timestamp: msgtimestamp,
                                    handshake: msghandshake,
                                    route:     proxy.route(),
                                    paths:     paths,
                                }).map(|v| (v, proxy))
                            }).and_then(move |(resp, proxy)| {
                                if resp.ok {
                                    let ft = futures::stream::once(Ok(proto::ConnectResponse {
                                        ok:        true,
                                        handshake: resp.handshake,
                                        route:     proxy.route(),
                                        paths:     resp.paths,
                                    }));
                                    let never = futures::future::empty().into_stream();
                                    let never = futurize::mark_stream(never, proxy);
                                    let ft = ft.chain(never);
                                    Ok(Box::new(ft) as Box<Stream<Item = _, Error = _> + Send + Sync>)
                                } else {
                                    let ft = futures::stream::once(Ok(proto::ConnectResponse {
                                        ok:        false,
                                        handshake: Vec::new(),
                                        route:     0,
                                        paths:     Vec::new(),
                                    }));
                                    Ok(Box::new(ft) as Box<Stream<Item = _, Error = _> + Send + Sync>)
                                }
                            });

                        Ok(Box::new(ft.into_stream().flatten()) as Box<Stream<Item = _, Error = _> + Sync + Send>)
                    } else {
                        Ok(Box::new(futures::stream::once(Ok(proto::ConnectResponse {
                            //TODO wait some time here to make it indistinguishable from client reject
                            ok:        false,
                            handshake: Vec::new(),
                            route:     0,
                            paths:     Vec::new(),
                        })))
                            as Box<Stream<Item = _, Error = _> + Sync + Send>)
                    }
                })
            }).flatten_stream();

        Ok(Box::new(ft))
    }
}
