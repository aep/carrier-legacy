#![recursion_limit = "128"]
extern crate carrier;
extern crate clap;
extern crate env_logger;
extern crate failure;
extern crate futures;
extern crate rand;
extern crate tokio;
#[macro_use]
extern crate log;
extern crate base64;
extern crate bytes;
extern crate hpack;
extern crate serde;
extern crate serde_json;
extern crate systemstat;
#[macro_use]
extern crate serde_derive;

extern crate tokio_file_unix;
extern crate tokio_codec;
extern crate tokio_pty_process;
extern crate passwd;
extern crate libc;
extern crate axon;
extern crate which;
extern crate tokio_fs;
extern crate sha2;

use carrier::*;
use clap::{App, Arg, SubCommand};
use failure::Error;
use futures::{Future, Sink, Stream};
use std::env;
use std::net::SocketAddr;
use bytes::Bytes;
use std::io;
use std::collections::HashMap;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "android",
))]
mod shell;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "android",
))]
mod system_stats;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "android",
))]
mod forward;

#[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "android",
))]
mod axons;

mod sft;
mod setup;
mod framed;

pub fn main_() -> Result<(), Error> {
    if let Err(_) = env::var("RUST_LOG") {
        env::set_var("RUST_LOG", "carrier=info");
    }
    env_logger::init();

    let clap = App::new("carrier cli")
        .version("0.7")
        .author("(2018) Arvid E. Picciani <arvid@devguard.io>")
        .setting(clap::AppSettings::ArgRequiredElseHelp)
        .setting(clap::AppSettings::UnifiedHelpMessage)
        .after_help(
            "SECRETS:
    Secrets have to be 64 bytes of high quality randomness.
    By default the secret is loaded from ~/.devguard/secret.
    You can generate a new secret by running
        $ carrier gen
    The secrets file can also be set in an environment variable
        $ export CARRIER_SECRET_FILE=~/.devguard/secret
    ",
        ).subcommand(SubCommand::with_name("gen").about("generate new identity"))

        .subcommand(SubCommand::with_name("mkshadow").about("create a shadow address"))
        .subcommand(
            SubCommand::with_name("sync")
                .about("coordinate a broker epoch")
                .arg(Arg::with_name("broker").takes_value(true).required(true).index(1))
                .arg(Arg::with_name("epoch").takes_value(true).required(true).index(2))
        ).subcommand(
            SubCommand::with_name("dns")
                .about("create dns record")
                .arg(Arg::with_name("priority").takes_value(true).required(true).index(2))
                .arg(Arg::with_name("epoch").takes_value(true).required(true).index(1))
                .arg(Arg::with_name("ip").takes_value(true).required(true).index(3)),
        ).subcommand(
            SubCommand::with_name("subscribe")
                .about("watch a shadow")
                .arg(Arg::with_name("address").takes_value(true).required(true).index(1))
        ).subcommand(
            SubCommand::with_name("archon")
                .about("spawn archon executable")
                .arg(Arg::with_name("index").takes_value(true).required(true).index(1))
        ).subcommand(
            SubCommand::with_name("update")
                .about("update a remote carrier")
                .arg(Arg::with_name("target").takes_value(true).required(true).index(1))
        ).subcommand(
            SubCommand::with_name("install")
                .about("install axon service on this system")
                .arg(
                    Arg::with_name("shadow")
                    .help("address of shadow to publish on")
                    .takes_value(true)
                    .required(true)
                    .index(1),
                    )
                .arg(
                    Arg::with_name("allow")
                    .long("allow")
                    .help("Allow access to identity")
                    .takes_value(true)
                    .multiple(true)
                    .required(true)
                    .value_names(&["identity"]),
                    ),
        ).subcommand(
            SubCommand::with_name("push")
                .about("stupid file transfer")
                .arg(Arg::with_name("target").takes_value(true).required(true).index(1))
                .arg(Arg::with_name("local-file").takes_value(true).required(true).index(2))
                .arg(Arg::with_name("remote-file").takes_value(true).required(true).index(3)),
        ).subcommand(
            SubCommand::with_name("get")
                .about("get something")
                .arg(Arg::with_name("target").takes_value(true).required(true).index(1))
                .arg(Arg::with_name("resource").takes_value(true).required(true).index(2))
                .arg(Arg::with_name("headers")
                     .long("header")
                     .short("H")
                     .takes_value(true)
                     .multiple(true)
                     .number_of_values(2)
                     .value_names(&["key", "value"])
                     .required(false))
        ).subcommand(
            SubCommand::with_name("ephermal")
                .about("generate a signed ephermal identity")
                .arg(
                    Arg::with_name("epoch")
                        .long("epoch")
                        .help("The current epoch in which to create certificate")
                        .takes_value(true)
                        .validator(|v| v.parse::<u32>().map_err(|e| format!("{}", e)).map(|_| ()))
                        .required(true),
                ).arg(
                    Arg::with_name("allow-delegation")
                        .long("allow-delegation")
                        .help("Allow to create more sub-certificates"),
                ).arg(
                    Arg::with_name("assume-identity")
                        .long("assume-identity")
                        .help("new certificate has the same identity"),
                ).arg(
                    Arg::with_name("text")
                        .long("text")
                        .help("write human readable interpretation to stdout"),
                ).arg(
                    Arg::with_name("access")
                        .long("access")
                        .help("Allow access to a targets resource in a shadow")
                        .multiple(true)
                        .number_of_values(3)
                        .value_names(&["shadow", "target", "resource"]),
                ),
        ).subcommand(SubCommand::with_name("identity").about("print public identity"))

        ;

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "android",
    ))]
    let clap = clap.subcommand(
        SubCommand::with_name("axon")
        .alias("axiom")
        .about("publish axon service on carrier")
        .arg(
            Arg::with_name("config")
            .help("read config from this location instead of /opt/devguard/axon.toml")
            .takes_value(true)
            .required(false)
            .index(1),
            )
    ).subcommand(
            SubCommand::with_name("forward")
                .about("connect a local tcp port to a remote port")
                .arg(
                    Arg::with_name("target")
                        .help("identity of axon service")
                        .takes_value(true)
                        .required(true)
                        .index(1),
                )
                .arg(
                    Arg::with_name("local")
                        .help("local port")
                        .takes_value(true)
                        .required(true)
                        .index(2),
                )
                .arg(
                    Arg::with_name("remote")
                        .help("remote port")
                        .takes_value(true)
                        .required(true)
                        .index(3),
                ),
    ).subcommand(
            SubCommand::with_name("shell")
                .about("get a shell on an axon service")
                .arg(
                    Arg::with_name("target")
                        .help("identity of axon service")
                        .takes_value(true)
                        .required(true)
                        .index(1),
                ),
        );


    let matches = clap.get_matches();

    match matches.subcommand() {
        ("gen", Some(_submatches)) => {
            println!("{}", keystore::Secrets::gen()?.identity.identity());
            Ok(())
        }
        ("mkshadow", Some(_submatches)) => {
            use rand::RngCore;

            let mut secret = vec![0; 32];
            let mut rng = rand::OsRng::new().expect("os rng");
            rng.try_fill_bytes(&mut secret).expect("rng fill");
            let secret = Secret::from_bytes(&mut secret).expect("secret from rng");

            let address = secret.address();

            println!("address: {}", address.to_string());
            println!("secret:  {}", secret.to_string());
            Ok(())
        }
        ("update", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;

            let config = config::Config::load()?;
            let target = config.resolve_identity(submatches.value_of("target").unwrap().to_string()).expect("resolving identity from cli");

            tokio::run(futures::lazy(move || {
                update(secrets.identity, target).map_err(|e| error!("{}", e))
            }));
            Ok(())
        }
        ("install", Some(submatches)) => {

            keystore::Secrets::gen().ok();




            let shadow : identity::Address = submatches.value_of("shadow").unwrap().to_string().parse().expect("parsing shadow from cli");
            let shadow = shadow.to_string();
            let allowed: HashMap<String,String> = submatches
                .values_of("allow")
                .unwrap()
                .map(|v|{
                    let v : identity::Identity = v.parse().expect("parsing identity from cli");
                    (v.to_string(), "*".into())
                })
                .collect();



            let aconf = axons::Config{
                publish: axons::PublisherConfig {
                    shadow,
                },
                allowed,
                keepalive: None,
            };

            setup::install(aconf)?;

            let secrets = keystore::Secrets::load()?;
            println!("service installed with identity {}", secrets.identity.identity());
            Ok(())
        }
        ("identity", Some(_submatches)) => {
            let secrets = keystore::Secrets::load()?;
            println!("{}", secrets.identity.identity());
            Ok(())
        }
        #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "android",
        ))]
        ("axon", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;
            let config_file = submatches
                .value_of("config")
                .map(|v|v.to_string())
                .unwrap_or("/opt/devguard/axon.toml".into());

            tokio::run(futures::lazy(move || {
                axons::axon(secrets.identity, config_file).map_err(|e| error!("{}", e))
            }));
            Ok(())
        }
        ("subscribe", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;
            let shadow  = submatches.value_of("address").unwrap().to_string().parse().expect("parsing shadow");

            tokio::run(futures::lazy(move || {
                subscribe(secrets.identity, shadow).map_err(|e| error!("{}", e))
            }));
            Ok(())
        }
        ("push", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;

            let config = config::Config::load()?;
            let target = config
                .resolve_identity(submatches.value_of("target").unwrap().to_string()).expect("resolving identity from cli");

            let local_file = submatches.value_of("local-file").unwrap().to_string();
            let remote_file = submatches.value_of("remote-file").unwrap().to_string();

            tokio::run(futures::lazy(move || {
                push(secrets.identity, target, local_file, remote_file).map_err(|e| error!("{}", e))
            }));
            Ok(())
        }
        ("get", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;

            let config = config::Config::load()?;
            let target = config
                .resolve_identity(submatches.value_of("target").unwrap().to_string()).expect("resolving identity from cli");
            let resource = submatches.value_of("resource").unwrap().to_string();


            let mut headers = headers::Headers::with_path(resource.as_bytes());
            let mut k = None;

            if let Some(h) = submatches.values_of("headers") {
                for h in h {
                    if k.is_none() {
                        k = Some(h);
                    } else {
                        headers.add(k.unwrap().as_bytes().to_vec(), h.as_bytes().to_vec());
                        k = None;
                    }
                }
            }

            tokio::run(futures::lazy(move || {
                get(secrets.identity, target, resource, headers).map_err(|e| error!("{}", e))
            }));
            Ok(())
        }
        #[cfg(any(
                target_os = "linux",
                target_os = "macos",
        ))]
        ("forward", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;
            let config = config::Config::load()?;
            let target = config
                .resolve_identity(submatches.value_of("target").unwrap().to_string())
                .expect("resolving identity from cli");


            let local  :u16 = submatches.value_of("local").unwrap().to_string().parse().unwrap();
            let remote :u16 = submatches.value_of("remote").unwrap().to_string().parse().unwrap();

            tokio::run(futures::lazy(move || {
                forward::forward(secrets.identity, target, local, remote).map_err(|e| error!("{}", e))
            }));
            Ok(())
        }
        #[cfg(any(
                target_os = "linux",
                target_os = "macos",
        ))]
        ("shell", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;
            let config = config::Config::load()?;
            let target = config
                .resolve_identity(submatches.value_of("target").unwrap().to_string())
                .expect("resolving identity from cli");
            tokio::run(futures::lazy(move || {
                shell(secrets.identity, target).map_err(|e| error!("{}", e))
            }));
            Ok(())
        }
        ("dns", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;

            let priority: u8 = submatches.value_of("priority").unwrap().parse().expect("parsing priority from cli");
            let addr: SocketAddr = submatches.value_of("ip").unwrap().parse().expect("parsing ip from cli");
            let x = secrets.identity.address();
            let epoch: u32 = submatches.value_of("epoch").unwrap().parse().expect("parsing epoch from cli");

            let dns = dns::DnsRecord {
                priority,
                addr,
                x,
                epoch,
            };

            println!("\"{}\"", dns.to_signed_txt(&secrets.identity));
            Ok(())
        }
        ("archon", Some(submatches)) => {
            let index = submatches.value_of("index").unwrap().to_string();
            match index.as_ref() {
                "/v0/system_stats" => {
                    system_stats::_main();
                },
                "/v0/sft" => {
                    sft::_main();
                },
                "/v0/shell" => {
                    shell::_main();
                },
                "/v0/connect" => {
                    forward::_main();
                },
                any => {
                    panic!("cannot find index {}. archon is not actually implemented yet", any);
                }
            }
            Ok(())
        }
        ("sync", Some(submatches)) => {
            let secrets = keystore::Secrets::load()?;
            let broker: std::net::IpAddr = submatches.value_of("broker").unwrap().to_string().parse().expect("broker ip");
            let epoch: u64 = submatches.value_of("epoch").unwrap().to_string().parse().expect("epoch");

            tokio::run(futures::lazy(move || {
                sync(secrets.identity, broker, epoch).map_err(|e| error!("{}", e))
            }));
            Ok(())
        }
        _ => unreachable!(),
    }
}

pub fn main() {
    match main_() {
        Ok(_) => (),
        Err(e) => {
            error!("{}", e);
            std::process::exit(4);
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
))]
pub fn shell(secret: identity::Secret, target: identity::Identity) -> impl Future<Item = (), Error = Error> {
    let domain = env::var("CARRIER_BROKER_DOMAIN").unwrap_or("2.carrier.devguard.io".to_string());
    connect::connect(domain, secret.clone()).and_then(move |(ep, mut brk, sock, addr)| {
        info!("established broker route {:#x} with {}", brk.route(), brk.identity());
        subscriber::connect(target, ep, &mut brk, sock, addr, secret).and_then(move |mut channel| {
            channel
                .open(headers::Headers::with_path("/v0/shell"))
                .expect("open channel")
                .into_future()
                .map_err(|(e, _)| e)
                .and_then(move |(headers, stream)| {
                    let headers = headers.expect("eof before header");
                    let headers = headers::Headers::decode(&headers).expect("decoding headers");
                    println!("{:?}", headers);
                    shell::ui(stream)
                }).and_then(|_| {
                    drop(brk);
                    drop(channel);
                    Ok(())
                })
        })
    })
}

pub fn subscribe(
    secret: identity::Secret,
    shadow: identity::Address,
) -> impl Future<Item = (), Error = Error> {
    let domain = env::var("CARRIER_BROKER_DOMAIN").unwrap_or("2.carrier.devguard.io".to_string());
    connect::connect(domain, secret.clone()).and_then(move |(ep, mut brk, sock, addr)| {
        brk.message("/carrier.broker.v1/broker/subscribe")
            .unwrap()
            .send(proto::SubscribeRequest {
                shadow: shadow.as_bytes().to_vec(),
                filter: Vec::new(),
            }).flatten_stream()
        .for_each(move |m: proto::SubscribeChange| {
            match m.m {
                Some(proto::subscribe_change::M::Publish(m)) => {
                    let identity = identity::Identity::from_bytes(&m.identity).expect("decoding identity");
                    info!("publish: {}", identity);
                },
                Some(proto::subscribe_change::M::Unpublish(m)) => {
                    let identity = identity::Identity::from_bytes(&m.identity).expect("decoding identity");
                    info!("unpublish: {}", identity);
                },
                Some(proto::subscribe_change::M::Supersede(_)) => {
                },
                None => (),
            }
            Ok(())
        })
        .and_then(|_| {
            drop(brk);
            Ok(())
        })
    })
}

pub fn push(
    secret: identity::Secret,
    target: identity::Identity,
    local_file: String,
    remote_file: String,
) -> impl Future<Item = (), Error = Error> {
    let domain = env::var("CARRIER_BROKER_DOMAIN").unwrap_or("2.carrier.devguard.io".to_string());

    use tokio_codec::BytesCodec;
    use tokio_codec::Decoder;

    let sha = {
        use std::fs::File;
        use std::io::Read;
        use sha2::{Sha256, Digest};
        let mut file = File::open(&local_file).expect(&format!("cannot open {}", &local_file));
        let mut hasher = Sha256::new();
        loop {
            let mut buf = vec![0;1024];
            let len = file.read(&mut buf).expect(&format!("cannot read {}", &local_file));
            if len == 0 {
                break;
            }
            hasher.input(&buf[..len]);
        }
        hasher.result().to_vec()
    };

    tokio_fs::file::File::open(local_file)
        .map_err(Error::from)
        .and_then(|local_file|{
        let local_file = framed::Framed(local_file);
        connect::connect(domain, secret.clone()).and_then(move |(ep, mut brk, sock, addr)| {
            info!("established broker route {:#x} with {}", brk.route(), brk.identity());
            subscriber::connect(target, ep, &mut brk, sock, addr, secret).and_then(move |mut channel| {
                let headers = headers::Headers::with_path("/v0/sft".as_bytes())
                    .and(":method".into(), "PUT".into())
                    .and("sha256".into(), sha)
                    .and("file".into(),   remote_file.into());
                channel
                    .open(headers)
                    .expect("open channel")
                    .into_future()
                    .map_err(|(e, _)| e)
                    .and_then(move |(headers, st)| {
                        let headers = headers.expect("eof before header");
                        let headers = headers::Headers::decode(&headers)?;
                        println!("{:?}", headers);
                        if headers.get(b":status") != Some(b"100") {
                            let errs = String::from_utf8_lossy(headers.get(b":error").unwrap_or(b"")).into_owned();
                            return Err(Error::from(io::Error::new(io::ErrorKind::Other, format!("remote error: {}", errs))));
                        }
                        Ok(local_file.map_err(Error::from).map(|i|i.into()).forward(st))
                    })
                    .flatten()
                    .and_then(|st|st.1.send(Bytes::new()))
                    .and_then(|st|st.into_future().map_err(|(e, _)| e))
                    .and_then(move |(headers, st)| {
                        let headers = headers.expect("eof before header");
                        let headers = headers::Headers::decode(&headers)?;
                        println!("{:?}", headers);
                        Ok(st)
                    })
                    .and_then(|_| {
                        drop(brk);
                        drop(channel);
                        Ok(())
                    })
            })
        })
    })

}

pub fn get(
    secret: identity::Secret,
    target: identity::Identity,
    resource: String,
    headers: headers::Headers,
) -> impl Future<Item = (), Error = Error> {
    let domain = env::var("CARRIER_BROKER_DOMAIN").unwrap_or("2.carrier.devguard.io".to_string());
    connect::connect(domain, secret.clone()).and_then(move |(ep, mut brk, sock, addr)| {
        info!("established broker route {:#x} with {}", brk.route(), brk.identity());
        subscriber::connect(target, ep, &mut brk, sock, addr, secret).and_then(move |mut channel| {
            channel
                .open(headers)
                .expect("open channel")
                .into_future()
                .map_err(|(e, _)| e)
                .and_then(move |(headers, st)| {
                    let headers = headers.expect("eof before header");
                    let headers = headers::Headers::decode(&headers)?;
                    println!("{:?}", headers);
                    Ok(st) as Result<_, Error>
                }).flatten_stream()
                .map_err(Error::from)
                .for_each(move |v: Bytes| {
                    println!("{}", String::from_utf8_lossy(v.as_ref()));
                    Ok(())
                }).and_then(|_| {
                    drop(brk);
                    drop(channel);
                    Ok(())
                })
        })
    })
}

pub fn update(
    secret: identity::Secret,
    target: identity::Identity,
) -> impl Future<Item = (), Error = Error> {
    let domain = env::var("CARRIER_BROKER_DOMAIN").unwrap_or("2.carrier.devguard.io".to_string());
    connect::connect(domain, secret.clone()).and_then(move |(ep, mut brk, sock, addr)| {
        info!("established broker route {:#x} with {}", brk.route(), brk.identity());
        subscriber::connect(target, ep, &mut brk, sock, addr, secret).and_then(move |mut channel| {
            channel
                .open(headers::Headers::with_path("/v0/self-update").and(":method".into(), "POST".into()))
                .expect("open channel")
                .into_future()
                .map_err(|(e, _)| e)
                .and_then(move |(headers, st)| {
                    let headers = headers.expect("eof before header");
                    let headers = headers::Headers::decode(&headers.to_vec())?;
                    println!("{:?}", headers);
                    Ok(st) as Result<_, Error>
                }).flatten_stream()
                .map_err(Error::from)
                .for_each(move |v: Bytes| {
                    println!("{}", String::from_utf8_lossy(v.as_ref()));
                    Ok(())
                }).and_then(|_| {
                    drop(brk);
                    drop(channel);
                    Ok(())
                })
        })
    })
}


pub fn sync(
    secret: identity::Secret,
    broker: std::net::IpAddr,
    epoch:  u64,
) -> impl Future<Item = (), Error = Error> {
    let domain = env::var("CARRIER_BROKER_DOMAIN").unwrap_or("2.carrier.devguard.io".to_string());
    connect::connect_to_ip(domain, broker, secret.clone()).and_then(move |(_ep, mut brk, _sock, _addr)| {
        brk.message("/carrier.broker.v1/broker/epochsync")
            .unwrap()
            .send(proto::EpochSyncRequest{
                epoch,
            }).flatten_stream()
        .for_each(move |m: proto::EpochSyncResponse| {
            if let Some(dump) = m.dump {
                println!("{:#?}", dump);
            }
            Ok(())
        })
        .and_then(|_| {
            drop(brk);
            Ok(())
        })
    })
}

